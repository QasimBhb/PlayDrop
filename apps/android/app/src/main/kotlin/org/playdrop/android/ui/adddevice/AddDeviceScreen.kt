package org.playdrop.android.ui.adddevice

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Computer
import androidx.compose.material.icons.filled.DevicesOther
import androidx.compose.material.icons.filled.Laptop
import androidx.compose.material.icons.filled.PhoneAndroid
import androidx.compose.material.icons.filled.Tablet
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.PrimaryTabRow
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.playdrop.android.R
import org.playdrop.android.qr.QrCodes
import org.playdrop.android.qr.QrScanner
import org.playdrop.android.qr.SecureWindow
import org.playdrop.android.ui.common.CollectMessages
import org.playdrop.android.ui.common.Format
import org.playdrop.android.ui.common.Samples
import org.playdrop.android.ui.common.UiMessage
import org.playdrop.android.ui.theme.MonoStyle
import org.playdrop.android.ui.theme.PlayDropTheme
import org.playdrop.core.AddDeviceCandidate

/** The Add device screen wired to its view model, the camera permission and the scanner. */
@Composable
fun AddDeviceRoute(onBack: () -> Unit, onHelp: () -> Unit, showMessage: (String) -> Unit, vm: AddDeviceViewModel = viewModel()) {
    val state by vm.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    CollectMessages(vm.messages, showMessage)
    LaunchedEffect(vm) { vm.done.collect { onBack() } }
    var cameraGranted by rememberSaveable {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED,
        )
    }
    var cameraDenied by rememberSaveable { mutableStateOf(false) }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        cameraGranted = granted
        cameraDenied = !granted
    }
    AddDeviceScreen(
        state = state,
        onBack = onBack,
        onMode = { mode ->
            vm.setMode(mode)
            if (mode == AddMode.SCAN_QR && !cameraGranted) permission.launch(Manifest.permission.CAMERA)
        },
        onPair = vm::pair,
        onPairTyped = vm::pairTyped,
        onReopen = vm::openWindow,
        onNewQr = vm::showQr,
        onHelp = onHelp,
        scanner = {
            when {
                cameraGranted -> QrScanner(
                    onPairUri = vm::onScanned,
                    onError = { showMessage(UiMessage(R.string.ui_add_camera_failed).resolve(context)) },
                    modifier = Modifier.fillMaxSize(),
                )
                else -> CameraNeeded(
                    denied = cameraDenied,
                    onAllow = { permission.launch(Manifest.permission.CAMERA) },
                )
            }
        },
    )
}

/** "Add device": devices found nearby, a typed address, show a QR code, or scan one. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AddDeviceScreen(
    state: AddDeviceUiState,
    onBack: () -> Unit,
    onMode: (AddMode) -> Unit,
    onPair: (AddDeviceCandidate) -> Unit,
    onPairTyped: (String) -> Unit,
    onReopen: () -> Unit,
    onNewQr: () -> Unit,
    onHelp: () -> Unit,
    scanner: @Composable () -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_add_device)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.ui_back))
                    }
                },
            )
        },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            PrimaryTabRow(selectedTabIndex = state.mode.ordinal) {
                Tab(state.mode == AddMode.NEARBY, onClick = { onMode(AddMode.NEARBY) }, text = { Text(stringResource(R.string.ui_add_tab_nearby)) })
                Tab(state.mode == AddMode.SHOW_QR, onClick = { onMode(AddMode.SHOW_QR) }, text = { Text(stringResource(R.string.ui_add_tab_show_qr)) })
                Tab(state.mode == AddMode.SCAN_QR, onClick = { onMode(AddMode.SCAN_QR) }, text = { Text(stringResource(R.string.ui_add_tab_scan_qr)) })
            }
            state.pairingWith?.let { name ->
                Row(Modifier.fillMaxWidth().padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                    Text(
                        if (name.isEmpty()) stringResource(R.string.ui_add_pairing) else stringResource(R.string.ui_add_pairing_with, name),
                        modifier = Modifier.padding(start = 12.dp),
                    )
                }
            }
            when (state.mode) {
                AddMode.NEARBY -> NearbyTab(state, onPair, onPairTyped, onReopen, onHelp)
                AddMode.SHOW_QR -> ShowQrTab(state, onNewQr)
                AddMode.SCAN_QR -> ScanQrTab(scanner)
            }
        }
    }
}

@Composable
private fun NearbyTab(
    state: AddDeviceUiState,
    onPair: (AddDeviceCandidate) -> Unit,
    onPairTyped: (String) -> Unit,
    onReopen: () -> Unit,
    onHelp: () -> Unit,
) {
    var typed by rememberSaveable { mutableStateOf("") }
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(stringResource(R.string.ui_add_intro), style = MaterialTheme.typography.bodyMedium)
        if (state.windowSecsLeft > 0) {
            Text(
                stringResource(R.string.ui_add_looking, Format.countdown(state.windowSecsLeft)),
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.primary,
            )
        } else {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(stringResource(R.string.ui_add_window_closed), modifier = Modifier.weight(1f))
                FilledTonalButton(onClick = onReopen) { Text(stringResource(R.string.ui_add_look_again)) }
            }
        }
        if (state.candidates.isEmpty()) {
            Text(
                stringResource(R.string.ui_add_none_yet),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            state.candidates.forEach { c ->
                val kind = CandidateKind.of(c.deviceType)
                ListItem(
                    leadingContent = { Icon(kindIcon(kind), contentDescription = null) },
                    headlineContent = { Text(c.name) },
                    supportingContent = { Text(stringResource(kindLabel(kind)) + " · " + c.addr, style = MonoStyle) },
                    trailingContent = {
                        Button(onClick = { onPair(c) }, enabled = state.pairingWith == null) {
                            Text(stringResource(R.string.ui_add_pair))
                        }
                    },
                    modifier = Modifier.clickable(enabled = state.pairingWith == null) { onPair(c) },
                )
            }
        }
        HorizontalDivider()
        Text(stringResource(R.string.ui_add_type_address), style = MaterialTheme.typography.titleSmall)
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(
                value = typed,
                onValueChange = { typed = it.take(64) },
                singleLine = true,
                placeholder = { Text(stringResource(R.string.ui_add_address_hint)) },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Go),
                keyboardActions = KeyboardActions(onGo = { onPairTyped(typed) }),
                modifier = Modifier.weight(1f),
            )
            Button(onClick = { onPairTyped(typed) }, enabled = typed.isNotBlank() && state.pairingWith == null) {
                Text(stringResource(R.string.ui_add_pair))
            }
        }
        TextButton(onClick = onHelp) { Text(stringResource(R.string.ui_help_link)) }
    }
}

@Composable
private fun ShowQrTab(state: AddDeviceUiState, onNewQr: () -> Unit) {
    SecureWindow()
    val uri = state.qrUri
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(stringResource(R.string.ui_qr_show_intro), style = MaterialTheme.typography.bodyMedium)
        if (uri != null) {
            val bitmap by produceState<ImageBitmap?>(null, uri) {
                value = withContext(Dispatchers.Default) { QrCodes.bitmap(uri, QR_PIXELS).asImageBitmap() }
            }
            Box(
                Modifier.widthIn(max = 320.dp).fillMaxWidth().aspectRatio(1f).background(Color.White),
                contentAlignment = Alignment.Center,
            ) {
                bitmap?.let { Image(it, contentDescription = stringResource(R.string.ui_qr_content_description), modifier = Modifier.fillMaxSize()) }
                    ?: CircularProgressIndicator()
            }
            Text(
                stringResource(R.string.ui_qr_valid_for, Format.countdown(state.qrSecsLeft)),
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.primary,
            )
            Text(
                stringResource(R.string.ui_qr_warning),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            Spacer(Modifier.size(24.dp))
            Text(stringResource(R.string.ui_qr_expired))
            Button(onClick = onNewQr) { Text(stringResource(R.string.ui_qr_new)) }
        }
    }
}

@Composable
private fun ScanQrTab(scanner: @Composable () -> Unit) {
    Column(Modifier.fillMaxSize().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(stringResource(R.string.ui_qr_scan_intro), style = MaterialTheme.typography.bodyMedium)
        Box(Modifier.fillMaxWidth().aspectRatio(1f), contentAlignment = Alignment.Center) { scanner() }
    }
}

@Composable
private fun CameraNeeded(denied: Boolean, onAllow: () -> Unit) {
    Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(
            stringResource(if (denied) R.string.ui_qr_camera_denied else R.string.ui_qr_camera_needed),
            style = MaterialTheme.typography.bodyMedium,
        )
        Button(onClick = onAllow) { Text(stringResource(R.string.ui_qr_allow_camera)) }
    }
}

private fun kindIcon(kind: CandidateKind) = when (kind) {
    CandidateKind.COMPUTER -> Icons.Filled.Computer
    CandidateKind.LAPTOP -> Icons.Filled.Laptop
    CandidateKind.PHONE -> Icons.Filled.PhoneAndroid
    CandidateKind.TABLET -> Icons.Filled.Tablet
    CandidateKind.OTHER -> Icons.Filled.DevicesOther
}

private fun kindLabel(kind: CandidateKind) = when (kind) {
    CandidateKind.COMPUTER -> R.string.ui_kind_computer
    CandidateKind.LAPTOP -> R.string.ui_kind_laptop
    CandidateKind.PHONE -> R.string.ui_kind_phone
    CandidateKind.TABLET -> R.string.ui_kind_tablet
    CandidateKind.OTHER -> R.string.ui_kind_other
}

private const val QR_PIXELS = 720

@Preview(showBackground = true)
@Composable
private fun AddDeviceNearbyPreview() {
    PlayDropTheme {
        AddDeviceScreen(Samples.addDeviceState, {}, {}, {}, {}, {}, {}, {}, scanner = {})
    }
}

@Preview(showBackground = true)
@Composable
private fun AddDeviceQrPreview() {
    PlayDropTheme {
        AddDeviceScreen(
            Samples.addDeviceState.copy(mode = AddMode.SHOW_QR, qrUri = "playdrop://pair?preview", qrSecsLeft = 95),
            {}, {}, {}, {}, {}, {}, {}, scanner = {},
        )
    }
}
