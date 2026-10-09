# JNA and the UniFFI-generated bindings are reached by reflection.
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-keep class org.playdrop.core.** { *; }
-dontwarn java.awt.*
