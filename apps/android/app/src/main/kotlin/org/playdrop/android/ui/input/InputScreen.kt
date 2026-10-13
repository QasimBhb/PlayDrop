package org.playdrop.android.ui.input

import android.os.SystemClock
import android.view.HapticFeedbackConstants
import android.view.WindowManager
import androidx.activity.compose.LocalActivity
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.NavigateBefore
import androidx.compose.material.icons.automirrored.filled.NavigateNext
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.PrimaryTabRow
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.Surface
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.input.pointer.changedToDownIgnoreConsumed
import androidx.compose.ui.input.pointer.changedToUpIgnoreConsumed
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChange
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleStartEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import org.playdrop.android.R
import org.playdrop.android.data.Settings
import org.playdrop.android.ui.common.CollectMessages
import org.playdrop.core.ButtonAction
import org.playdrop.core.InputEvent
import org.playdrop.core.PointerButton
import org.playdrop.core.PresenterAction
import org.playdrop.core.Modifier as KeyModifier

/**
 * The remote input screen wired to its view model. The session opens while the screen is
 * started and closes when it stops (another screen, the app in the background) or leaves;
 * the screen stays on while it is shown.
 */
@Composable
fun InputRoute(
    onBack: () -> Unit,
    showMessage: (String) -> Unit,
    vm: InputViewModel = viewModel(),
) {
    val state by vm.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    CollectMessages(vm.messages, showMessage)
    LaunchedEffect(vm) {
        vm.closed.collect {
            showMessage(it.resolve(context))
            onBack()
        }
    }
    LifecycleStartEffect(vm) {
        vm.start()
        onStopOrDispose { vm.stop() }
    }
    val activity = LocalActivity.current
    DisposableEffect(activity) {
        activity?.window?.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        onDispose { activity?.window?.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON) }
    }
    InputScreen(
        state = state,
        onBack = onBack,
        onSend = vm::send,
        onPresenter = vm::presenter,
        onSensitivity = vm::setSensitivity,
    )
}

private enum class InputTab(val title: Int) {
    TRACKPAD(R.string.ui_input_tab_trackpad),
    KEYBOARD(R.string.ui_input_tab_keyboard),
    PRESENTER(R.string.ui_input_tab_presenter),
}

/** Trackpad, keyboard and presenter tabs for one peer; tabs the peer cannot take are left out. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun InputScreen(
    state: InputUiState,
    onBack: () -> Unit,
    onSend: (List<InputEvent>) -> Unit,
    onPresenter: (PresenterAction) -> Unit,
    onSensitivity: (Float) -> Unit,
) {
    var menu by remember { mutableStateOf(false) }
    var speedDialog by rememberSaveable { mutableStateOf(false) }
    val tabs = buildList {
        if (state.features.pointer) add(InputTab.TRACKPAD)
        if (state.features.keys) add(InputTab.KEYBOARD)
        if (state.features.presenter) add(InputTab.PRESENTER)
    }
    var selected by rememberSaveable { mutableStateOf(0) }
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(stringResource(R.string.ui_input_title))
                        if (state.label.isNotEmpty()) {
                            Text(state.label, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.ui_back))
                    }
                },
                actions = {
                    if (state.features.pointer) {
                        IconButton(onClick = { menu = true }) {
                            Icon(Icons.Filled.MoreVert, contentDescription = stringResource(R.string.ui_input_more))
                        }
                        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.ui_input_speed)) },
                                onClick = {
                                    menu = false
                                    speedDialog = true
                                },
                            )
                        }
                    }
                },
            )
        },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            when {
                !state.online -> Notice(stringResource(R.string.ui_input_offline, state.label))
                tabs.isEmpty() -> Notice(stringResource(R.string.ui_input_unsupported))
                else -> {
                    val index = selected.coerceIn(0, tabs.lastIndex)
                    if (tabs.size > 1) {
                        PrimaryTabRow(selectedTabIndex = index) {
                            tabs.forEachIndexed { i, tab ->
                                Tab(selected = i == index, onClick = { selected = i }, text = { Text(stringResource(tab.title)) })
                            }
                        }
                    }
                    when (tabs[index]) {
                        InputTab.TRACKPAD -> TrackpadTab(state.sensitivity, onSend)
                        InputTab.KEYBOARD -> KeyboardTab(onSend)
                        InputTab.PRESENTER -> PresenterTab(state.features.keys, onSend, onPresenter)
                    }
                }
            }
        }
    }
    if (speedDialog) {
        SpeedDialog(state.sensitivity, onDone = {
            speedDialog = false
            onSensitivity(it)
        }, onDismiss = { speedDialog = false })
    }
}

@Composable
private fun Notice(text: String) {
    Text(text, modifier = Modifier.padding(24.dp), style = MaterialTheme.typography.bodyLarge)
}

/** Sends [events] and ticks the haptics for clicks and presses. */
@Composable
private fun rememberSender(onSend: (List<InputEvent>) -> Unit): (List<InputEvent>) -> Unit {
    val view = LocalView.current
    val send by rememberUpdatedState(onSend)
    return remember(view) {
        { events ->
            if (events.isNotEmpty()) {
                if (TrackpadGestures.wantsHaptic(events)) view.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
                send(events)
            }
        }
    }
}

@Composable
private fun TrackpadTab(sensitivity: Float, onSend: (List<InputEvent>) -> Unit) {
    val density = LocalDensity.current.density
    val gestures = remember(density) { TrackpadGestures(density, sensitivity) }
    SideEffect { gestures.sensitivity = sensitivity }
    val emit = rememberSender(onSend)
    Column(Modifier.fillMaxSize().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Surface(
            color = MaterialTheme.colorScheme.surfaceContainerHigh,
            shape = RoundedCornerShape(16.dp),
            modifier = Modifier
                .fillMaxWidth()
                .weight(1f)
                .border(1.dp, MaterialTheme.colorScheme.outlineVariant, RoundedCornerShape(16.dp))
                .pointerInput(gestures) {
                    awaitPointerEventScope {
                        try {
                            while (true) {
                                val deadline = gestures.longPressDeadline()
                                val event = if (deadline == null) {
                                    awaitPointerEvent()
                                } else {
                                    val wait = deadline - SystemClock.uptimeMillis()
                                    if (wait <= 0) null else withTimeoutOrNull(wait) { awaitPointerEvent() }
                                }
                                if (event == null) {
                                    emit(gestures.onLongPressTimeout(SystemClock.uptimeMillis()))
                                    continue
                                }
                                event.changes.forEach { c ->
                                    val id = c.id.value
                                    val out = when {
                                        c.changedToDownIgnoreConsumed() -> gestures.onDown(id, c.position.x, c.position.y, c.uptimeMillis)
                                        c.changedToUpIgnoreConsumed() -> gestures.onUp(id, c.uptimeMillis)
                                        c.positionChange() != Offset.Zero -> gestures.onMove(id, c.position.x, c.position.y)
                                        else -> emptyList()
                                    }
                                    emit(out)
                                    c.consume()
                                }
                            }
                        } finally {
                            emit(gestures.onCancel())
                        }
                    }
                },
        ) {
            Box(contentAlignment = Alignment.Center, modifier = Modifier.fillMaxSize().padding(24.dp)) {
                Text(
                    stringResource(R.string.ui_input_trackpad_hint),
                    textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
            listOf(
                PointerButton.LEFT to R.string.ui_input_left,
                PointerButton.MIDDLE to R.string.ui_input_middle,
                PointerButton.RIGHT to R.string.ui_input_right,
            ).forEach { (button, label) ->
                FilledTonalButton(
                    onClick = { emit(listOf(InputEvent.PointerButton(button = button, action = ButtonAction.CLICK))) },
                    modifier = Modifier.weight(1f).height(56.dp),
                ) { Text(stringResource(label)) }
            }
        }
    }
}

private val MODIFIERS = listOf(
    KeyModifier.CTRL to R.string.ui_input_ctrl,
    KeyModifier.ALT to R.string.ui_input_alt,
    KeyModifier.SHIFT to R.string.ui_input_shift,
    KeyModifier.META to R.string.ui_input_meta,
)

private val KEY_LABELS = mapOf(
    "Escape" to R.string.ui_key_escape,
    "Tab" to R.string.ui_key_tab,
    "Enter" to R.string.ui_key_enter,
    "Backspace" to R.string.ui_key_backspace,
    "Delete" to R.string.ui_key_delete,
    "ArrowLeft" to R.string.ui_key_left,
    "ArrowUp" to R.string.ui_key_up,
    "ArrowDown" to R.string.ui_key_down,
    "ArrowRight" to R.string.ui_key_right,
    "Home" to R.string.ui_key_home,
    "End" to R.string.ui_key_end,
    "PageUp" to R.string.ui_key_page_up,
    "PageDown" to R.string.ui_key_page_down,
    "AudioVolumeDown" to R.string.ui_key_volume_down,
    "AudioVolumeMute" to R.string.ui_key_mute,
    "AudioVolumeUp" to R.string.ui_key_volume_up,
    "MediaPlayPause" to R.string.ui_key_play_pause,
)

@Composable
private fun KeyboardTab(onSend: (List<InputEvent>) -> Unit) {
    val emit = rememberSender(onSend)
    val held = remember { mutableStateListOf<KeyModifier>() }
    var field by remember {
        mutableStateOf(TextFieldValue(KeyboardInput.SENTINEL, TextRange(KeyboardInput.SENTINEL.length)))
    }
    fun pressKey(name: String) {
        emit(listOf(KeyboardInput.key(name, held.toList())))
        held.clear()
    }
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        OutlinedTextField(
            value = field,
            onValueChange = { next ->
                val events = KeyboardInput.diff(field.text, next.text, held.toList())
                if (events.isNotEmpty()) {
                    emit(events)
                    held.clear()
                }
                val text = KeyboardInput.normalise(next.text)
                field = if (text == next.text) next else TextFieldValue(text, TextRange(text.length))
            },
            label = { Text(stringResource(R.string.ui_input_type_hint)) },
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrectEnabled = false,
            ),
            modifier = Modifier.fillMaxWidth(),
        )
        Text(stringResource(R.string.ui_input_mods), style = MaterialTheme.typography.labelLarge)
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            MODIFIERS.forEach { (mod, label) ->
                FilterChip(
                    selected = mod in held,
                    onClick = { if (mod in held) held.remove(mod) else held.add(mod) },
                    label = { Text(stringResource(label)) },
                )
            }
        }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            KeyboardInput.SPECIAL_KEYS.forEach { key ->
                OutlinedButton(onClick = { pressKey(key) }) { Text(stringResource(KEY_LABELS.getValue(key))) }
            }
        }
        Text(stringResource(R.string.ui_input_media_keys), style = MaterialTheme.typography.labelLarge)
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            KeyboardInput.MEDIA_KEYS.forEach { key ->
                FilledTonalButton(onClick = { pressKey(key) }) { Text(stringResource(KEY_LABELS.getValue(key))) }
            }
        }
    }
}

@Composable
private fun PresenterTab(
    keys: Boolean,
    onSend: (List<InputEvent>) -> Unit,
    onPresenter: (PresenterAction) -> Unit,
) {
    val emit = rememberSender(onSend)
    val view = LocalView.current
    var pointerOn by rememberSaveable { mutableStateOf(false) }
    if (keys) {
        val sendVolume by rememberUpdatedState<(Boolean) -> Unit> { up ->
            emit(listOf(KeyboardInput.key(if (up) KeyboardInput.VOLUME_UP else KeyboardInput.VOLUME_DOWN)))
        }
        DisposableEffect(Unit) {
            val handler: (Boolean) -> Unit = { up -> sendVolume(up) }
            VolumeKeys.capture(handler)
            onDispose { VolumeKeys.release(handler) }
        }
    }
    fun act(action: PresenterAction) {
        view.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
        onPresenter(action)
    }
    Column(Modifier.fillMaxSize().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Row(Modifier.fillMaxWidth().weight(1f), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            OutlinedButton(
                onClick = { act(PresenterAction.PREVIOUS) },
                shape = RoundedCornerShape(16.dp),
                modifier = Modifier.weight(1f).fillMaxSize(),
            ) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Icon(Icons.AutoMirrored.Filled.NavigateBefore, contentDescription = null)
                    Text(stringResource(R.string.ui_input_previous))
                }
            }
            Button(
                onClick = { act(PresenterAction.NEXT) },
                shape = RoundedCornerShape(16.dp),
                modifier = Modifier.weight(2f).fillMaxSize(),
            ) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Icon(Icons.AutoMirrored.Filled.NavigateNext, contentDescription = null)
                    Text(stringResource(R.string.ui_input_next), style = MaterialTheme.typography.titleLarge)
                }
            }
        }
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            FilledTonalButton(onClick = { act(PresenterAction.BLACKOUT) }, modifier = Modifier.weight(1f)) {
                Text(stringResource(R.string.ui_input_blackout))
            }
            FilterChip(
                selected = pointerOn,
                onClick = {
                    pointerOn = !pointerOn
                    act(if (pointerOn) PresenterAction.POINTER_ON else PresenterAction.POINTER_OFF)
                },
                label = { Text(stringResource(R.string.ui_input_pointer)) },
            )
        }
        if (keys) {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                listOf(KeyboardInput.VOLUME_DOWN, "AudioVolumeMute", KeyboardInput.VOLUME_UP).forEach { key ->
                    OutlinedButton(onClick = { emit(listOf(KeyboardInput.key(key))) }, modifier = Modifier.weight(1f)) {
                        Text(stringResource(KEY_LABELS.getValue(key)))
                    }
                }
            }
            Text(
                stringResource(R.string.ui_input_volume_keys),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun SpeedDialog(initial: Float, onDone: (Float) -> Unit, onDismiss: () -> Unit) {
    var value by remember { mutableFloatStateOf(initial) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.ui_input_speed)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(stringResource(R.string.ui_input_speed_body), style = MaterialTheme.typography.bodyMedium)
                Slider(
                    value = value,
                    onValueChange = { value = it },
                    valueRange = Settings.MIN_TRACKPAD_SENSITIVITY..Settings.MAX_TRACKPAD_SENSITIVITY,
                )
                Text(stringResource(R.string.ui_input_speed_value, value), style = MaterialTheme.typography.labelLarge)
            }
        },
        confirmButton = { TextButton(onClick = { onDone(value) }) { Text(stringResource(R.string.ui_input_done)) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.ui_cancel)) } },
    )
}
