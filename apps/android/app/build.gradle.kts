import org.gradle.process.ExecOperations
import javax.inject.Inject

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

// ---------------------------------------------------------------------------------------
// Rust: the core and its UniFFI surface live in the workspace two levels up. Two task
// types bridge them into this module through the variant API, so Gradle knows the
// generated Kotlin and the .so files are outputs and the configuration cache stays valid:
//   buildRust<Variant>   cargo ndk builds libplaydrop_ffi.so per ABI into build/rustJniLibs
//   generateBindings     builds the host library once and runs uniffi-bindgen in library
//                        mode to produce the Kotlin package org.playdrop.core
// Both need cargo, cargo-ndk and the NDK (ANDROID_HOME/ndk). ~/.cargo/bin is added to PATH.
// ---------------------------------------------------------------------------------------

val workspaceDir: File = rootProject.projectDir.parentFile.parentFile
val rustAbis: List<String> = (project.findProperty("playdrop.rustAbis") as String)
    .split(",").map { it.trim() }.filter { it.isNotEmpty() }

abstract class CargoTask : DefaultTask() {
    @get:Inject abstract val execOps: ExecOperations
    @get:Internal abstract val workspace: DirectoryProperty
    @get:InputDirectory abstract val ffiSources: DirectoryProperty
    @get:InputDirectory abstract val coreSources: DirectoryProperty
    @get:OutputDirectory abstract val outputDir: DirectoryProperty

    protected fun cargo(vararg args: String) {
        val home = System.getProperty("user.home")
        val cargo = System.getenv("CARGO") ?: "$home/.cargo/bin/cargo"
        execOps.exec {
            workingDir = workspace.get().asFile
            environment("PATH", listOf("$home/.cargo/bin", System.getenv("PATH") ?: "").joinToString(":"))
            commandLine(listOf(cargo) + args.toList())
        }
    }
}

/** cargo ndk build of the FFI library for every ABI in `playdrop.rustAbis`. */
abstract class BuildRustTask : CargoTask() {
    @get:Input abstract val abis: ListProperty<String>
    @get:Input abstract val release: Property<Boolean>

    @TaskAction
    fun build() {
        val args = mutableListOf("ndk", "-P", "26")
        abis.get().forEach { args += listOf("-t", it) }
        args += listOf("-o", outputDir.get().asFile.absolutePath, "build", "-p", "playdrop-ffi")
        if (release.get()) args += "--release"
        cargo(*args.toTypedArray())
    }
}

/** Host build of the FFI library plus uniffi-bindgen in library mode. */
abstract class GenerateBindingsTask : CargoTask() {
    @TaskAction
    fun generate() {
        cargo("build", "-p", "playdrop-ffi")
        val hostLib = workspace.get().asFile.resolve("target/debug/libplaydrop_ffi.so")
        cargo(
            "run", "-p", "uniffi-bindgen", "--", "generate",
            "--library", hostLib.absolutePath,
            "--language", "kotlin", "--no-format",
            "--out-dir", outputDir.get().asFile.absolutePath,
        )
    }
}

val generateBindings = tasks.register<GenerateBindingsTask>("generateBindings") {
    description = "Builds playdrop-ffi for the host and generates the Kotlin bindings"
    group = "rust"
    workspace.set(workspaceDir)
    ffiSources.set(workspaceDir.resolve("crates/playdrop-ffi/src"))
    coreSources.set(workspaceDir.resolve("crates/playdrop-core/src"))
    outputDir.set(layout.buildDirectory.dir("generated/uniffi/kotlin"))
}

android {
    namespace = "org.playdrop.android"
    compileSdk = 37
    // The NDK cargo-ndk uses; AGP needs the same one to find llvm-strip for the .so files.
    ndkVersion = "27.1.12297006"

    defaultConfig {
        applicationId = "org.playdrop.android"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        ndk {
            abiFilters += rustAbis
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            // Until a release key exists (Phase 6), release builds are signed with the debug
            // key so the optimised Rust core can be measured on a phone.
            signingConfig = signingConfigs.getByName("debug")
        }
        debug {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    packaging {
        jniLibs {
            useLegacyPackaging = false
        }
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
    }

}

androidComponents {
    onVariants { variant ->
        val cap = variant.name.replaceFirstChar { it.uppercase() }
        val buildRust = tasks.register<BuildRustTask>("buildRust$cap") {
            description = "cargo ndk build of libplaydrop_ffi.so for ${rustAbis.joinToString()} (${variant.name})"
            group = "rust"
            workspace.set(workspaceDir)
            ffiSources.set(workspaceDir.resolve("crates/playdrop-ffi/src"))
            coreSources.set(workspaceDir.resolve("crates/playdrop-core/src"))
            abis.set(rustAbis)
            release.set(variant.buildType == "release")
            outputDir.set(layout.buildDirectory.dir("rustJniLibs/${variant.name}"))
        }
        variant.sources.jniLibs?.addGeneratedSourceDirectory(buildRust, BuildRustTask::outputDir)
        variant.sources.kotlin?.addGeneratedSourceDirectory(generateBindings, GenerateBindingsTask::outputDir)
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.runtime.ktx)
    implementation(libs.androidx.lifecycle.service)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.navigation.compose)
    implementation(libs.androidx.datastore.preferences)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.ui.graphics)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.compose.material.icons.extended)
    implementation(libs.androidx.camera.core)
    implementation(libs.androidx.camera.camera2)
    implementation(libs.androidx.camera.lifecycle)
    implementation(libs.androidx.camera.view)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.zxing.core)
    implementation("${libs.jna.get()}@aar")

    testImplementation(libs.junit)
    androidTestImplementation(libs.androidx.junit)
    androidTestImplementation(libs.androidx.espresso.core)
    androidTestImplementation(platform(libs.androidx.compose.bom))
    androidTestImplementation(libs.androidx.compose.ui.test.junit4)
    debugImplementation(libs.androidx.compose.ui.tooling)
    debugImplementation(libs.androidx.compose.ui.test.manifest)
}
