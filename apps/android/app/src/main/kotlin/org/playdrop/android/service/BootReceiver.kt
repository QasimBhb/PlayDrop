package org.playdrop.android.service

/**
 * Starts the service after boot, once the user finished onboarding ([AutoStartReceiver]).
 * `connectedDevice` may start from `BOOT_COMPLETED` (unlike `dataSync` on Android 15). The
 * action is checked because only the system can send `BOOT_COMPLETED`; anything else is ignored.
 */
class BootReceiver : AutoStartReceiver(AutoStart.BOOT, "after boot")
