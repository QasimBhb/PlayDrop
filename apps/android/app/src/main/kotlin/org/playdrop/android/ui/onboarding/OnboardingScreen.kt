package org.playdrop.android.ui.onboarding

import android.Manifest
import android.app.Application
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Bolt
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material.icons.filled.PhonelinkRing
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.data.Settings
import org.playdrop.android.ui.common.CollectMessages
import org.playdrop.android.ui.common.CoreViewModel
import org.playdrop.android.ui.common.UiMessage
import org.playdrop.android.ui.theme.PlayDropTheme

/** Finishes first-run: starts the service and marks onboarding done. */
class OnboardingViewModel(application: Application) : CoreViewModel(application) {
    private val starting = MutableStateFlow(false)

    /** True while the service is starting. */
    val busy: StateFlow<Boolean> = starting.asStateFlow()

    /**
     * Starts the foreground service, waits for the core, then sets `onboarded`. Onboarding
     * ends even if the start fails; the Devices tab then shows the Start banner.
     */
    fun finish() {
        if (starting.value) return
        starting.value = true
        viewModelScope.launch {
            val ok = guarded { CoreHost.awaitRunning(getApplication()) }
            if (ok == false) post(UiMessage(R.string.ui_onboarding_start_slow))
            try {
                Settings.update(getApplication()) { it.copy(onboarded = true) }
            } catch (e: Throwable) {
                post(UiMessage.fromOrRethrow(e))
            }
            starting.value = false
        }
    }
}

/** First-run screens wired to the permission request and [OnboardingViewModel]. */
@Composable
fun OnboardingRoute(showMessage: (String) -> Unit, vm: OnboardingViewModel = viewModel()) {
    val busy by vm.busy.collectAsStateWithLifecycle()
    CollectMessages(vm.messages, showMessage)
    var step by rememberSaveable { mutableIntStateOf(0) }
    val notifications = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { step = 2 }
    OnboardingScreen(
        step = step,
        busy = busy,
        onNext = { step = (step + 1).coerceAtMost(2) },
        onAllowNotifications = {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                notifications.launch(Manifest.permission.POST_NOTIFICATIONS)
            } else {
                step = 2
            }
        },
        onStart = vm::finish,
    )
}

/** Three steps: what PlayDrop does, notifications, start the service. */
@Composable
fun OnboardingScreen(
    step: Int,
    busy: Boolean,
    onNext: () -> Unit,
    onAllowNotifications: () -> Unit,
    onStart: () -> Unit,
) {
    Surface(Modifier.fillMaxSize()) {
        Column(Modifier.safeDrawingPadding().padding(24.dp)) {
            LinearProgressIndicator(progress = { (step + 1) / 3f }, modifier = Modifier.fillMaxWidth())
            Column(
                Modifier.weight(1f).fillMaxWidth(),
                verticalArrangement = Arrangement.Center,
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                when (step) {
                    0 -> Step(Icons.Filled.PhonelinkRing, R.string.ui_onboarding_1_title, R.string.ui_onboarding_1_body)
                    1 -> Step(Icons.Filled.Notifications, R.string.ui_onboarding_2_title, R.string.ui_onboarding_2_body)
                    else -> Step(Icons.Filled.Bolt, R.string.ui_onboarding_3_title, R.string.ui_onboarding_3_body)
                }
            }
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End, verticalAlignment = Alignment.CenterVertically) {
                when (step) {
                    0 -> Button(onClick = onNext) { Text(stringResource(R.string.ui_next)) }
                    1 -> {
                        TextButton(onClick = onNext) { Text(stringResource(R.string.ui_not_now)) }
                        Spacer(Modifier.size(8.dp))
                        Button(onClick = onAllowNotifications) { Text(stringResource(R.string.ui_allow)) }
                    }
                    else -> Button(onClick = onStart, enabled = !busy) {
                        if (busy) {
                            CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                        } else {
                            Text(stringResource(R.string.ui_onboarding_start))
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun Step(icon: ImageVector, title: Int, body: Int) {
    Icon(icon, contentDescription = null, modifier = Modifier.size(72.dp), tint = MaterialTheme.colorScheme.primary)
    Spacer(Modifier.size(24.dp))
    Text(stringResource(title), style = MaterialTheme.typography.headlineSmall, textAlign = TextAlign.Center)
    Spacer(Modifier.size(12.dp))
    Text(
        stringResource(body),
        style = MaterialTheme.typography.bodyLarge,
        textAlign = TextAlign.Center,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Preview(showBackground = true)
@Composable
private fun OnboardingPreview() {
    PlayDropTheme { OnboardingScreen(step = 0, busy = false, onNext = {}, onAllowNotifications = {}, onStart = {}) }
}

@Preview(showBackground = true)
@Composable
private fun OnboardingNotificationsPreview() {
    PlayDropTheme { OnboardingScreen(step = 1, busy = false, onNext = {}, onAllowNotifications = {}, onStart = {}) }
}
