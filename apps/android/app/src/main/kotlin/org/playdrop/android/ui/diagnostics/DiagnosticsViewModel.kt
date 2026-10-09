package org.playdrop.android.ui.diagnostics

import android.app.Application
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import org.playdrop.android.ui.common.CoreViewModel
import org.playdrop.core.Diagnostics

/** Everything the Diagnostics screen shows. */
data class DiagnosticsUiState(
    /** A snapshot is being taken. */
    val loading: Boolean = false,
    /** The last snapshot, if any. */
    val report: Diagnostics? = null,
    /** The plain-language findings for [report]. */
    val summary: List<DiagIssue> = emptyList(),
)

/** Takes `diagnose()` snapshots on entry and on Refresh. */
class DiagnosticsViewModel(application: Application) : CoreViewModel(application) {
    private val mutable = MutableStateFlow(DiagnosticsUiState())

    /** The screen state. */
    val state: StateFlow<DiagnosticsUiState> = mutable.asStateFlow()

    init {
        refresh()
    }

    /** Takes a new snapshot. */
    fun refresh() {
        if (mutable.value.loading) return
        mutable.update { it.copy(loading = true) }
        viewModelScope.launch {
            val report = core { it.diagnose() }
            mutable.update {
                if (report == null) {
                    it.copy(loading = false)
                } else {
                    DiagnosticsUiState(loading = false, report = report, summary = DiagnosticsSummary.of(report))
                }
            }
        }
    }
}
