package org.playdrop.android.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.SwapVert
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.NavGraph.Companion.findStartDestination
import androidx.navigation.NavHostController
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import kotlinx.coroutines.launch
import org.playdrop.android.R
import org.playdrop.android.ui.adddevice.AddDeviceRoute
import org.playdrop.android.ui.common.CollectMessages
import org.playdrop.android.ui.device.DeviceDetailRoute
import org.playdrop.android.ui.device.DeviceDetailViewModel
import org.playdrop.android.ui.devices.DevicesRoute
import org.playdrop.android.ui.diagnostics.DiagnosticsRoute
import org.playdrop.android.ui.onboarding.OnboardingRoute
import org.playdrop.android.ui.pairing.PairingDialog
import org.playdrop.android.ui.settings.SettingsRoute
import org.playdrop.android.ui.transfers.TransfersRoute
import org.playdrop.android.ui.transfers.TransfersViewModel

/** Navigation routes. Device IDs are hex, so they are safe in a path segment. */
object Routes {
    /** Devices tab (start). */
    const val DEVICES = "devices"

    /** Transfers tab. */
    const val TRANSFERS = "transfers"

    /** Settings tab. */
    const val SETTINGS = "settings"

    /** Device detail, with the hex Device ID as argument. */
    const val DEVICE = "device/{${DeviceDetailViewModel.ARG_ID}}"

    /** Add device. */
    const val ADD_DEVICE = "add-device"

    /** Diagnostics. */
    const val DIAGNOSTICS = "diagnostics"

    /** The detail route for one device. */
    fun device(id: String) = "device/$id"

    /** Routes that show the bottom bar. */
    val TOP_LEVEL = setOf(DEVICES, TRANSFERS, SETTINGS)
}

private data class Tab(val route: String, val label: Int, val icon: ImageVector)

private val TABS = listOf(
    Tab(Routes.DEVICES, R.string.ui_tab_devices, Icons.Filled.Devices),
    Tab(Routes.TRANSFERS, R.string.ui_tab_transfers, Icons.Filled.SwapVert),
    Tab(Routes.SETTINGS, R.string.ui_tab_settings, Icons.Filled.Settings),
)

/**
 * The app's root: onboarding until it is done, then the three tabs with Add device, Device
 * detail and Diagnostics on top, the pairing dialog over everything, and one snackbar host.
 * [openTab] selects a tab once (for example from a notification).
 */
@Composable
fun PlayDropRoot(appVm: AppViewModel, openTab: String?, onTabOpened: () -> Unit) {
    val transfersVm: TransfersViewModel = viewModel()
    val settings by appVm.settings.collectAsStateWithLifecycle()
    val prompt by appVm.pairingPrompt.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    val scope = rememberCoroutineScope()
    val show: (String) -> Unit = { text -> scope.launch { snackbar.showSnackbar(text) } }
    CollectMessages(appVm.messages, show)

    val s = settings
    when {
        s == null -> Box(Modifier.fillMaxSize())
        !s.onboarded -> Scaffold(snackbarHost = { SnackbarHost(snackbar) }) { padding ->
            Box(Modifier.padding(padding).consumeWindowInsets(padding)) { OnboardingRoute(show) }
        }
        else -> MainScaffold(transfersVm, snackbar, show, openTab, onTabOpened)
    }
    prompt?.let { PairingDialog(it, appVm::answerPairing) }
}

@Composable
private fun MainScaffold(
    transfersVm: TransfersViewModel,
    snackbar: SnackbarHostState,
    show: (String) -> Unit,
    openTab: String?,
    onTabOpened: () -> Unit,
) {
    val nav = rememberNavController()
    val entry by nav.currentBackStackEntryAsState()
    val route = entry?.destination?.route
    val pending by transfersVm.pendingOffers.collectAsStateWithLifecycle()

    LaunchedEffect(openTab) {
        if (openTab != null && openTab in Routes.TOP_LEVEL) {
            nav.selectTab(openTab)
            onTabOpened()
        }
    }

    Scaffold(
        contentWindowInsets = WindowInsets(0),
        snackbarHost = { SnackbarHost(snackbar) },
        bottomBar = {
            if (route == null || route in Routes.TOP_LEVEL) {
                NavigationBar {
                    TABS.forEach { tab ->
                        NavigationBarItem(
                            selected = route == tab.route,
                            onClick = { nav.selectTab(tab.route) },
                            icon = {
                                if (tab.route == Routes.TRANSFERS && pending > 0) {
                                    BadgedBox(badge = { Badge { Text(pending.toString()) } }) {
                                        Icon(tab.icon, contentDescription = null)
                                    }
                                } else {
                                    Icon(tab.icon, contentDescription = null)
                                }
                            },
                            label = { Text(stringResource(tab.label)) },
                        )
                    }
                }
            }
        },
    ) { padding ->
        NavHost(
            navController = nav,
            startDestination = Routes.DEVICES,
            modifier = Modifier.padding(padding).consumeWindowInsets(padding),
        ) {
            composable(Routes.DEVICES) {
                DevicesRoute(
                    onOpenDevice = { nav.navigate(Routes.device(it)) },
                    onAddDevice = { nav.navigate(Routes.ADD_DEVICE) },
                    showMessage = show,
                )
            }
            composable(Routes.TRANSFERS) { TransfersRoute(transfersVm, show) }
            composable(Routes.SETTINGS) {
                SettingsRoute(onOpenDiagnostics = { nav.navigate(Routes.DIAGNOSTICS) }, showMessage = show)
            }
            composable(
                Routes.DEVICE,
                arguments = listOf(navArgument(DeviceDetailViewModel.ARG_ID) { type = NavType.StringType }),
            ) {
                DeviceDetailRoute(
                    onBack = { nav.popIfAt(Routes.DEVICE) },
                    onPairAgain = { nav.navigate(Routes.ADD_DEVICE) },
                    showMessage = show,
                )
            }
            composable(Routes.ADD_DEVICE) {
                AddDeviceRoute(onBack = { nav.popIfAt(Routes.ADD_DEVICE) }, showMessage = show)
            }
            composable(Routes.DIAGNOSTICS) {
                DiagnosticsRoute(onBack = { nav.popIfAt(Routes.DIAGNOSTICS) }, showMessage = show)
            }
        }
    }
}

/** Switches tabs the Material way: one copy of each tab, state saved and restored. */
private fun NavHostController.selectTab(route: String) {
    navigate(route) {
        popUpTo(graph.findStartDestination().id) { saveState = true }
        launchSingleTop = true
        restoreState = true
    }
}

/** Pops only while [route] is on top, so a repeated back event never leaves the app's start screen. */
private fun NavHostController.popIfAt(route: String) {
    if (currentDestination?.route == route) popBackStack()
}
