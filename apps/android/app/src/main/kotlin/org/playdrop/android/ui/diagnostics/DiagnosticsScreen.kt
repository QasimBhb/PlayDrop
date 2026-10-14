package org.playdrop.android.ui.diagnostics

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Error
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import org.playdrop.android.R
import org.playdrop.android.ui.common.CollectMessages
import org.playdrop.android.ui.common.Format
import org.playdrop.android.ui.common.Samples
import org.playdrop.android.ui.help.HelpTopic
import org.playdrop.android.ui.theme.MonoStyle
import org.playdrop.android.ui.theme.OnlineGreen
import org.playdrop.android.ui.theme.PlayDropTheme
import org.playdrop.core.Diagnostics
import org.playdrop.core.DiscoveryPath

/** The Diagnostics screen wired to its view model. */
@Composable
fun DiagnosticsRoute(onBack: () -> Unit, onHelp: (HelpTopic?) -> Unit, showMessage: (String) -> Unit, vm: DiagnosticsViewModel = viewModel()) {
    val state by vm.state.collectAsStateWithLifecycle()
    CollectMessages(vm.messages, showMessage)
    DiagnosticsScreen(state, onBack, vm::refresh, onHelp)
}

/** The summary first, then each layer: listener, interfaces, multicast, mDNS, paired devices. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DiagnosticsScreen(state: DiagnosticsUiState, onBack: () -> Unit, onRefresh: () -> Unit, onHelp: (HelpTopic?) -> Unit) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_diagnostics_title)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.ui_back))
                    }
                },
                actions = {
                    if (state.loading) {
                        CircularProgressIndicator(Modifier.size(24.dp).padding(end = 4.dp), strokeWidth = 2.dp)
                    } else {
                        IconButton(onClick = onRefresh) {
                            Icon(Icons.Filled.Refresh, contentDescription = stringResource(R.string.ui_refresh))
                        }
                    }
                },
            )
        },
    ) { padding ->
        val report = state.report
        Column(
            Modifier.padding(padding).fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (report == null) {
                Text(
                    stringResource(if (state.loading) R.string.ui_diag_running else R.string.ui_diag_unavailable),
                    style = MaterialTheme.typography.bodyMedium,
                )
                return@Column
            }
            val fine = state.summary == listOf(DiagIssue.AllFine)
            Card(
                colors = CardDefaults.cardColors(
                    containerColor = if (fine) MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.errorContainer,
                ),
            ) {
                Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    state.summary.forEach { Text(issueText(it), style = MaterialTheme.typography.bodyMedium) }
                    val topic = DiagnosticsSummary.helpTopic(state.summary)
                    if (DiagnosticsSummary.suggestsHelp(state.summary)) {
                        Text(
                            stringResource(
                                when (topic) {
                                    HelpTopic.SAME_NETWORK -> R.string.ui_diag_help_suggest_same
                                    HelpTopic.OTHER_NETWORK -> R.string.ui_diag_help_suggest_other
                                    null -> R.string.ui_diag_help_suggest_generic
                                },
                            ),
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        TextButton(onClick = { onHelp(topic) }) { Text(stringResource(R.string.ui_help_link)) }
                    }
                }
            }
            Sections(report)
            if (!DiagnosticsSummary.suggestsHelp(state.summary)) {
                TextButton(onClick = { onHelp(null) }) { Text(stringResource(R.string.ui_help_link)) }
            }
        }
    }
}

@Composable
private fun Sections(d: Diagnostics) {
    Section(
        stringResource(R.string.ui_diag_listener),
        d.bindOk,
        listOf(
            if (d.bindOk) stringResource(R.string.ui_diag_listening_on, d.addr) else stringResource(R.string.ui_diag_not_listening, d.port.toInt()),
        ),
    )
    Section(
        stringResource(R.string.ui_diag_interfaces),
        d.interfaces.isNotEmpty(),
        if (d.interfaces.isEmpty()) {
            listOf(stringResource(R.string.ui_diag_no_interface))
        } else {
            d.interfaces.map { stringResource(R.string.ui_diag_interface_line, it.addr, it.netmask, it.index.toInt()) }
        },
    )
    val m = d.multicast
    Section(
        stringResource(R.string.ui_diag_multicast),
        m.enabled && m.joinFailed.isEmpty() && m.beaconRecently,
        if (!m.enabled) {
            listOf(stringResource(R.string.ui_diag_off))
        } else {
            m.joined.map { stringResource(R.string.ui_diag_joined_on, it) } +
                m.joinFailed.map { stringResource(R.string.ui_diag_join_failed, it) } +
                listOf(
                    m.lastBeaconSecsAgo?.let { stringResource(R.string.ui_diag_beacon_ago, Format.duration(it.toLong())) }
                        ?: stringResource(R.string.ui_diag_no_beacon),
                )
        },
    )
    val n = d.mdns
    Section(
        stringResource(R.string.ui_diag_mdns),
        n.enabled && n.registered && n.error == null,
        if (!n.enabled) {
            listOf(stringResource(R.string.ui_diag_off))
        } else {
            listOfNotNull(
                stringResource(if (n.registered) R.string.ui_diag_mdns_registered else R.string.ui_diag_mdns_not_registered),
                n.error?.let { stringResource(R.string.ui_diag_error, it) },
                n.lastSeenSecsAgo?.let { stringResource(R.string.ui_diag_advert_ago, Format.duration(it.toLong())) }
                    ?: stringResource(R.string.ui_diag_no_advert),
            )
        },
    )
    if (d.peers.isNotEmpty()) {
        val lines = ArrayList<String>()
        var ok = true
        for (p in d.peers) {
            lines += stringResource(
                R.string.ui_diag_peer_line,
                p.label,
                p.shortId,
                stringResource(if (p.online) R.string.ui_device_online else R.string.ui_diag_offline),
                stringResource(if (p.mdnsSeen) R.string.ui_yes else R.string.ui_no),
                stringResource(if (p.beaconSeen) R.string.ui_yes else R.string.ui_no),
            )
            p.lastResult?.let {
                ok = ok && p.online
                lines += stringResource(R.string.ui_diag_peer_failure, pathName(it.path), it.failure)
            }
        }
        Section(stringResource(R.string.ui_diag_peers), ok, lines)
    }
}

@Composable
private fun Section(title: String, ok: Boolean, lines: List<String>) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(
                if (ok) Icons.Filled.CheckCircle else Icons.Filled.Error,
                contentDescription = stringResource(if (ok) R.string.ui_diag_ok else R.string.ui_diag_problem),
                tint = if (ok) OnlineGreen else MaterialTheme.colorScheme.error,
                modifier = Modifier.size(18.dp),
            )
            Text(title, style = MaterialTheme.typography.titleSmall, modifier = Modifier.padding(start = 8.dp))
        }
        lines.forEach { Text(it, style = MonoStyle, modifier = Modifier.padding(start = 26.dp)) }
    }
}

@Composable
private fun pathName(p: DiscoveryPath): String = stringResource(
    when (p) {
        DiscoveryPath.MDNS -> R.string.ui_path_mdns
        DiscoveryPath.BEACON -> R.string.ui_path_beacon
        DiscoveryPath.LAST_KNOWN -> R.string.ui_path_last_known
    },
)

@Composable
private fun issueText(i: DiagIssue): String = when (i) {
    is DiagIssue.NotListening -> stringResource(R.string.ui_issue_not_listening, i.port)
    DiagIssue.NoInterface -> stringResource(R.string.ui_issue_no_interface)
    is DiagIssue.MulticastJoinFailed -> stringResource(R.string.ui_issue_multicast_join, i.addr, i.error)
    is DiagIssue.MulticastBlocked -> stringResource(R.string.ui_issue_multicast_blocked, i.ifaceAddr, i.peerLabel)
    DiagIssue.DiscoveryOff -> stringResource(R.string.ui_issue_discovery_off)
    is DiagIssue.MdnsFailed -> stringResource(R.string.ui_issue_mdns, i.error)
    is DiagIssue.OtherNetwork -> stringResource(R.string.ui_issue_other_network, i.peerLabel)
    is DiagIssue.PeerNotReached -> stringResource(R.string.ui_issue_peer, i.peerLabel, pathName(i.path), i.failure)
    DiagIssue.AllFine -> stringResource(R.string.ui_issue_all_fine)
}

@Preview(showBackground = true, heightDp = 1000)
@Composable
private fun DiagnosticsPreview() {
    PlayDropTheme { DiagnosticsScreen(Samples.diagnosticsState, {}, {}, {}) }
}
