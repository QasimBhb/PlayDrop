package org.playdrop.android.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.unit.sp

private val Teal = Color(0xFF00696E)
private val TealLight = Color(0xFF4CD9E0)

private val LightColors = lightColorScheme(primary = Teal, secondary = Color(0xFF4A6365))
private val DarkColors = darkColorScheme(primary = TealLight, secondary = Color(0xFFB1CBCD))

/** Colour for the "online" dot; the same in both themes so it reads as status, not brand. */
val OnlineGreen = Color(0xFF2E9D4F)

/** Monospace style for Short IDs and addresses (SR-8: easy to compare character by character). */
val MonoStyle = TextStyle(fontFamily = FontFamily.Monospace, fontSize = 13.sp, fontWeight = FontWeight.Medium)

/**
 * The app theme: Material 3 with the wallpaper colours on Android 12 and later and a teal
 * scheme before that.
 */
@Composable
fun PlayDropTheme(dark: Boolean = isSystemInDarkTheme(), content: @Composable () -> Unit) {
    val context = LocalContext.current
    val colors = when {
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S ->
            if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        dark -> DarkColors
        else -> LightColors
    }
    MaterialTheme(colorScheme = colors, content = content)
}
