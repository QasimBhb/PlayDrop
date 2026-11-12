package org.playdrop.android.share

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Only another app's `content://` items may be shared to PlayDrop (T34). */
class ShareUrisTest {
    private val own = setOf("org.playdrop.android.files", "org.playdrop.android.androidx-startup")

    @Test
    fun anotherAppsContentItemPasses() {
        assertTrue(ShareUris.allowed("content", "com.google.android.apps.photos.contentprovider", own))
        assertTrue(ShareUris.allowed("content", "media", own))
    }

    @Test
    fun fileItemsAndPlayDropsOwnProvidersAreRefused() {
        assertFalse(ShareUris.allowed("file", null, own))
        assertFalse(ShareUris.allowed("file", "", own))
        assertFalse(ShareUris.allowed("content", "org.playdrop.android.files", own))
        assertFalse(ShareUris.allowed("content", "org.playdrop.android.androidx-startup", own))
        assertFalse(ShareUris.allowed("content", null, own))
        assertFalse(ShareUris.allowed("http", "example.org", own))
        assertFalse(ShareUris.allowed(null, "media", own))
    }
}
