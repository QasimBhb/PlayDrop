package org.playdrop.android.ui.mirror

import android.app.Application
import android.content.Intent
import android.content.pm.PackageManager
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.playdrop.android.R
import org.playdrop.android.data.Settings
import org.playdrop.android.service.NotificationMirror
import org.playdrop.android.ui.theme.PlayDropTheme

/** What the app picker shows. */
data class MirrorAppsUiState(
    /** Null while the installed apps are being read. */
    val apps: List<MirrorApp>? = null,
    /** Packages on the allow list. */
    val allowed: Set<String> = emptySet(),
) {
    /** Allowed packages the query did not find, shown with a Remove button. */
    val notFound: List<String>
        get() = apps?.let { list -> MirrorApps.notFound(allowed, list.mapTo(HashSet()) { it.packageName }) }.orEmpty()
}

/**
 * Lists the apps that have a launcher entry (visible through the manifest's `<queries>`, no
 * `QUERY_ALL_PACKAGES`) and edits the allow list in [Settings.Values.mirrorApps].
 */
class MirrorAppsViewModel(application: Application) : AndroidViewModel(application) {
    private val apps = MutableStateFlow<List<MirrorApp>?>(null)

    /** The screen state. */
    val state: StateFlow<MirrorAppsUiState> =
        combine(apps, Settings.flow(application).map { it.mirrorApps }) { list, allowed -> MirrorAppsUiState(list, allowed) }
            .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), MirrorAppsUiState())

    init {
        viewModelScope.launch {
            val app = getApplication<Application>()
            val found = withContext(Dispatchers.IO) { launcherApps(app) }
            apps.value = MirrorApps.forPicker(found, app.packageName)
        }
    }

    /** Adds [packageName] to the allow list or takes it off. */
    fun setAllowed(packageName: String, on: Boolean) {
        viewModelScope.launch {
            Settings.update(getApplication()) { it.copy(mirrorApps = MirrorApps.toggled(it.mirrorApps, packageName, on)) }
            // Taken off the list: its copies leave every desktop (SR-42).
            if (!on) NotificationMirror.withdrawApp(packageName)
        }
    }

    private fun launcherApps(app: Application): List<MirrorApp> {
        val pm = app.packageManager
        val intent = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        return pm.queryIntentActivities(intent, PackageManager.ResolveInfoFlags.of(0)).map {
            MirrorApp(it.activityInfo.packageName, it.loadLabel(pm).toString())
        }
    }
}

/** The app picker wired to its view model. */
@Composable
fun MirrorAppsRoute(onBack: () -> Unit, vm: MirrorAppsViewModel = viewModel()) {
    val state by vm.state.collectAsStateWithLifecycle()
    MirrorAppsScreen(state, onBack, vm::setAllowed)
}

/** Launcher apps sorted by name, a checkbox each; ticked apps may be mirrored (SR-41). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MirrorAppsScreen(state: MirrorAppsUiState, onBack: () -> Unit, onToggle: (String, Boolean) -> Unit) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_mirror_apps_title)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.ui_back))
                    }
                },
            )
        },
    ) { padding ->
        val apps = state.apps
        if (apps == null) {
            Box(Modifier.padding(padding).fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            return@Scaffold
        }
        LazyColumn(Modifier.padding(padding).fillMaxSize()) {
            item {
                Text(
                    stringResource(R.string.ui_mirror_apps_body),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(16.dp),
                )
            }
            if (apps.isEmpty()) {
                item { Text(stringResource(R.string.ui_mirror_apps_empty), modifier = Modifier.padding(16.dp)) }
            }
            val missing = state.notFound
            if (missing.isNotEmpty()) {
                item {
                    Text(
                        stringResource(R.string.ui_mirror_apps_not_found_header),
                        style = MaterialTheme.typography.titleSmall,
                        modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 8.dp),
                    )
                }
                items(missing, key = { "missing:$it" }) { pkg ->
                    ListItem(
                        headlineContent = { Text(pkg) },
                        supportingContent = { Text(stringResource(R.string.ui_mirror_apps_not_found)) },
                        trailingContent = {
                            TextButton(onClick = { onToggle(pkg, false) }) { Text(stringResource(R.string.ui_mirror_apps_remove)) }
                        },
                    )
                }
            }
            items(apps, key = { it.packageName }) { app ->
                val checked = app.packageName in state.allowed
                ListItem(
                    headlineContent = { Text(app.label) },
                    trailingContent = { Checkbox(checked = checked, onCheckedChange = { onToggle(app.packageName, it) }) },
                    modifier = Modifier.clickable { onToggle(app.packageName, !checked) },
                )
            }
        }
    }
}

@Preview(showBackground = true)
@Composable
private fun MirrorAppsPreview() {
    PlayDropTheme {
        MirrorAppsScreen(
            MirrorAppsUiState(
                apps = listOf(MirrorApp("org.example.chat", "Chat"), MirrorApp("org.example.mail", "Mail")),
                allowed = setOf("org.example.chat"),
            ),
            onBack = {},
            onToggle = { _, _ -> },
        )
    }
}
