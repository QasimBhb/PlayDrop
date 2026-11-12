package org.playdrop.android.share

import android.os.Build
import androidx.activity.compose.BackHandler
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.selection.triStateToggleable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TriStateCheckbox
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.state.ToggleableState
import androidx.compose.ui.unit.dp
import org.playdrop.android.R

/** The send card: a bottom card over a dim scrim; the scrim and back cancel. */
@Composable
internal fun SendSheetContent(sheet: SendSheet) {
    val state = sheet.state
    // SR-29, T34: a tap that reaches the chooser's Send or a card's Cancel through a window
    // drawn over this one (tapjacking) is dropped, for as long as this card is up.
    val view = LocalView.current
    DisposableEffect(view) {
        val before = view.filterTouchesWhenObscured
        view.filterTouchesWhenObscured = true
        onDispose { view.filterTouchesWhenObscured = before }
    }
    if (state == SheetState.Hidden) return
    ShareTheme {
        BackHandler { sheet.cancel() }
        Box(
            Modifier
                .fillMaxSize()
                .background(Color.Black.copy(alpha = 0.32f))
                .clickable(onClick = sheet::cancel),
            contentAlignment = Alignment.BottomCenter,
        ) {
            Card(
                Modifier
                    .fillMaxWidth()
                    .navigationBarsPadding()
                    // A Material3 card consumes its own taps, so only the scrim cancels.
                    .padding(16.dp),
            ) {
                when (state) {
                    SheetState.Hidden -> Unit
                    SheetState.Preparing -> PreparingCard()
                    is SheetState.Countdown -> TimedCard(state.title, sheet::cancel)
                    is SheetState.Sending -> TimedCard(state.title, sheet::cancel)
                    is SheetState.Choose -> ChooserCard(state, sheet)
                }
            }
        }
    }
}

@Composable
private fun PreparingCard() {
    Row(Modifier.padding(24.dp), verticalAlignment = Alignment.CenterVertically) {
        CircularProgressIndicator(Modifier.size(24.dp))
        Spacer(Modifier.padding(8.dp))
        Text(stringResource(R.string.share_preparing))
    }
}

/** A title, a bar that fills over the cancel window, and Cancel. */
@Composable
private fun TimedCard(title: String, onCancel: () -> Unit) {
    var started by remember(title) { mutableStateOf(false) }
    val progress by animateFloatAsState(
        targetValue = if (started) 1f else 0f,
        animationSpec = tween(SendSheet.CANCEL_WINDOW_MS.toInt(), easing = LinearEasing),
        label = "countdown",
    )
    LaunchedEffect(title) { started = true }
    Column(Modifier.padding(20.dp)) {
        Text(title, style = MaterialTheme.typography.titleMedium)
        Spacer(Modifier.height(16.dp))
        LinearProgressIndicator(progress = { progress }, modifier = Modifier.fillMaxWidth())
        Spacer(Modifier.height(8.dp))
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
            TextButton(onClick = onCancel) { Text(stringResource(R.string.cancel)) }
        }
    }
}

@Composable
private fun ChooserCard(state: SheetState.Choose, sheet: SendSheet) {
    val selection = state.selection
    Column(Modifier.padding(vertical = 16.dp)) {
        Row(
            Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                chooserTitle(state.kind, state.items),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.weight(1f),
            )
            if (selection.canToggleAll) SelectAll(selection, sheet)
        }
        LazyColumn(Modifier.heightIn(max = 360.dp)) {
            items(selection.rows, key = { it.id }) { row ->
                ChooserRowItem(row, state.kind, row.id in selection.ticked, sheet)
            }
        }
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 12.dp),
            horizontalArrangement = Arrangement.End,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TextButton(onClick = sheet::cancel) { Text(stringResource(R.string.cancel)) }
            Spacer(Modifier.width(8.dp))
            Button(onClick = sheet::send, enabled = state.canSend) {
                Text(
                    if (selection.canSend) {
                        pluralStringResource(R.plurals.share_send_to_devices, selection.count, selection.count)
                    } else {
                        stringResource(R.string.share_send)
                    },
                )
            }
        }
    }
}

@Composable
private fun chooserTitle(kind: SendKind, items: Int): String = when (kind) {
    SendKind.FILES -> pluralStringResource(R.plurals.share_choose_files, items, items)
    SendKind.SHARED_TEXT -> stringResource(R.string.share_choose_text)
    SendKind.CLIPBOARD_TEXT, SendKind.CLIPBOARD_IMAGE -> stringResource(R.string.share_choose_clipboard)
}

/**
 * "Select all": one tick box for every row that can be ticked, read out as a check box that
 * is checked, not checked or partly checked. A tap ticks all of them, or clears them once all
 * are ticked.
 */
@Composable
private fun SelectAll(selection: Selection, sheet: SendSheet) {
    val state = when {
        selection.allTicked -> ToggleableState.On
        selection.ticked.isEmpty() -> ToggleableState.Off
        else -> ToggleableState.Indeterminate
    }
    val description = tickState(state)
    Row(
        Modifier
            .triStateToggleable(state = state, role = Role.Checkbox, onClick = sheet::toggleAll)
            .semantics { stateDescription = description }
            .padding(start = 12.dp, end = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(stringResource(R.string.share_select_all), style = MaterialTheme.typography.labelLarge)
        // The row is the toggle, so the box itself takes no separate tap.
        TriStateCheckbox(state = state, onClick = null, modifier = Modifier.padding(12.dp))
    }
}

/** The spoken state of a tick box. */
@Composable
private fun tickState(state: ToggleableState): String = stringResource(
    when (state) {
        ToggleableState.On -> R.string.share_state_ticked
        ToggleableState.Off -> R.string.share_state_not_ticked
        ToggleableState.Indeterminate -> R.string.share_state_some_ticked
    },
)

@Composable
private fun ChooserRowItem(row: ChooserRow, kind: SendKind, ticked: Boolean, sheet: SendSheet) {
    val description = tickState(ToggleableState(ticked))
    Row(
        Modifier.fillMaxWidth().padding(start = 8.dp, end = 4.dp, top = 4.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // The box and the names are one check box for accessibility services: checkable,
        // checked or not, and greyed out with the row. The menu stays its own button.
        Row(
            Modifier
                .weight(1f)
                .toggleable(value = ticked, enabled = row.selectable, role = Role.Checkbox) { sheet.toggle(row.id) }
                .semantics { stateDescription = description },
            verticalAlignment = Alignment.CenterVertically,
        ) {
            ChooserRowBody(row, kind, ticked)
        }
        if (!row.isDefault) RowMenu(row, sheet)
    }
}

/** The tick box, the name and the detail line of a chooser row. */
@Composable
private fun RowScope.ChooserRowBody(row: ChooserRow, kind: SendKind, ticked: Boolean) {
    // The row is the toggle, so the box itself takes no separate tap.
    Checkbox(checked = ticked, onCheckedChange = null, enabled = row.selectable, modifier = Modifier.padding(12.dp))
    Column(Modifier.weight(1f).alpha(if (row.selectable) 1f else 0.6f)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(row.label, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f, fill = false))
            if (row.isDefault) {
                Spacer(Modifier.width(8.dp))
                Text(
                    stringResource(R.string.share_default_marker),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
        }
        Text(rowDetail(row, kind), style = MaterialTheme.typography.bodySmall)
    }
}

/** The Short ID (SR-8), plus why the row is greyed out or what happens when it is offline. */
@Composable
private fun rowDetail(row: ChooserRow, kind: SendKind): String {
    val status = when (row.blocked) {
        Blocked.REMOVED -> stringResource(R.string.share_row_removed)
        Blocked.OFFLINE -> stringResource(R.string.share_row_offline)
        Blocked.CLIPBOARD_OFF -> stringResource(R.string.share_row_clipboard_off)
        Blocked.NO_IMAGES -> stringResource(R.string.share_row_no_images)
        null -> when {
            row.online -> null
            kind == SendKind.FILES -> stringResource(R.string.share_row_offline_files)
            else -> stringResource(R.string.share_row_offline_text)
        }
    }
    return if (status == null) row.shortId else stringResource(R.string.share_row_detail, row.shortId, status)
}

@Composable
private fun RowMenu(row: ChooserRow, sheet: SendSheet) {
    var open by remember { mutableStateOf(false) }
    Box {
        IconButton(onClick = { open = true }) {
            Icon(Icons.Filled.MoreVert, contentDescription = stringResource(R.string.share_row_more, row.label))
        }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            DropdownMenuItem(
                text = { Text(stringResource(R.string.share_make_default)) },
                onClick = {
                    open = false
                    sheet.makeDefault(row.id)
                },
            )
        }
    }
}

/** The share card's theme: the system's dynamic colours where available. */
@Composable
private fun ShareTheme(content: @Composable () -> Unit) {
    val dark = isSystemInDarkTheme()
    val ctx = LocalContext.current
    val scheme = when {
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> if (dark) dynamicDarkColorScheme(ctx) else dynamicLightColorScheme(ctx)
        dark -> darkColorScheme()
        else -> lightColorScheme()
    }
    MaterialTheme(colorScheme = scheme, content = content)
}
