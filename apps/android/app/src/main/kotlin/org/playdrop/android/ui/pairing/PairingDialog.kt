package org.playdrop.android.ui.pairing

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.DialogProperties
import org.playdrop.android.R
import org.playdrop.android.core.PendingPairing
import org.playdrop.android.ui.common.Format
import org.playdrop.android.ui.common.Samples
import org.playdrop.android.ui.common.ShortIdText
import org.playdrop.android.ui.theme.PlayDropTheme
import org.playdrop.core.PairingPrompt

/**
 * The pairing question, shown over any screen. For SAS pairing it shows the six digits, the
 * name and the Short ID (SR-8); for a QR scan on the other side it asks to add the device
 * (SR-9). Neither button is focused by default and tapping outside does nothing; Back
 * rejects. The SAS is never logged (SR-32).
 */
@Composable
fun PairingDialog(pending: PendingPairing, onAnswer: (Boolean) -> Unit) {
    val sas = pending.prompt as? PairingPrompt.Sas
    AlertDialog(
        onDismissRequest = { onAnswer(false) },
        properties = DialogProperties(dismissOnClickOutside = false, dismissOnBackPress = true),
        title = {
            Text(
                if (sas != null) {
                    stringResource(R.string.ui_pair_sas_title, pending.name)
                } else {
                    stringResource(R.string.ui_pair_confirm_title, pending.name, pending.shortId)
                },
            )
        },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                if (sas != null) {
                    Text(stringResource(R.string.ui_pair_sas_body))
                    Text(
                        Format.sas(sas.sas),
                        fontSize = 44.sp,
                        fontWeight = FontWeight.Bold,
                        fontFamily = FontFamily.Monospace,
                        letterSpacing = 4.sp,
                        textAlign = TextAlign.Center,
                        modifier = Modifier.fillMaxWidth(),
                    )
                } else {
                    Text(stringResource(R.string.ui_pair_confirm_body))
                }
                Text(stringResource(R.string.ui_pair_name, pending.name), style = MaterialTheme.typography.bodyMedium)
                ShortIdText(pending.shortId)
            }
        },
        confirmButton = {
            TextButton(onClick = { onAnswer(true) }) {
                Text(stringResource(if (sas != null) R.string.ui_pair_accept else R.string.ui_pair_add))
            }
        },
        dismissButton = {
            TextButton(onClick = { onAnswer(false) }) {
                Text(stringResource(if (sas != null) R.string.ui_pair_reject else R.string.ui_cancel))
            }
        },
    )
}

@Preview
@Composable
private fun PairingSasPreview() {
    PlayDropTheme { PairingDialog(Samples.sasPrompt) {} }
}

@Preview
@Composable
private fun PairingConfirmPreview() {
    PlayDropTheme { PairingDialog(Samples.sasPrompt.copy(prompt = PairingPrompt.ScannerConfirm)) {} }
}
