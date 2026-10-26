package org.playdrop.android.core

import java.io.File

/**
 * Where a copied file ended up: [promisedUri] is the location the core stored when the file
 * landed, [actualUri] the real one (null only if the platform gave none), [renumbered] true
 * when the file did not get its promised name.
 */
internal data class Placement(val promisedUri: String, val actualUri: String?, val renumbered: Boolean)

/**
 * The platform half of a move, split off so [LandedMove] runs on the JVM: it copies [file]
 * into `Download/PlayDrop` (direct write or MediaStore, journaled) and reports where it went.
 * It must remove any half-written copy before it throws, and it never deletes [file].
 */
internal fun interface DownloadsPlacer {
    fun place(landed: LandingPaths.Landed, file: File): Placement
}

/**
 * The order of one landed-file move, free of Android types so unit tests can drive it:
 * validate the path, copy ([DownloadsPlacer]), record where the file went, delete the landed
 * file, forget the journal. The reasons for each step are on [DownloadsSink.moveLanded],
 * which delegates here.
 */
internal class LandedMove(
    private val landingRoot: () -> File?,
    private val downloadsFile: (LandingPaths.Landed) -> File,
    private val placer: DownloadsPlacer,
    private val relocations: Relocations,
    private val journal: MoveJournal,
    private val claims: NameClaims,
    /** Called when [relocations] gained or lost an entry, so the pass flushes the table once. */
    private val onRelocationsChanged: () -> Unit,
    private val info: (String) -> Unit,
    private val warn: (String, Throwable?) -> Unit,
) {
    fun move(transferId: String, file: File): MoveResult {
        val root = landingRoot() ?: return MoveResult(MoveOutcome.FAILED)
        val landed = LandingTree.relative(root, file)?.let(LandingPaths::parse)
        if (landed == null || landed.transferId != transferId) {
            warn("a file in the landing folder has a path PlayDrop would not write; left in place", null)
            return MoveResult(MoveOutcome.FAILED)
        }
        val target = downloadsFile(landed)
        if (!file.exists()) {
            claims.release(target.path, file.path)
            return MoveResult(MoveOutcome.GONE)
        }
        try {
            val placement = placer.place(landed, file)
            // Before the landed file is deleted: a kill after the delete must not lose where the
            // file went, and a file that got its promised name must drop any older entry (a
            // redone move). If the table cannot be updated the file stays landed and the pass
            // stops (the journal entry is kept, the next pass removes this copy and redoes it):
            // deleting it would leave a stale location that could open someone else's file.
            if (placement.renumbered || !relocations.isEmpty()) {
                if (!relocations.noteMove(transferId, placement.promisedUri, placement.actualUri, placement.renumbered)) {
                    warn("could not update the relocation table; the move is redone later", null)
                    return MoveResult(MoveOutcome.FAILED, stopPass = true)
                }
                onRelocationsChanged()
            }
            // Delete before forgetting the journal. If the delete fails the entry stays and
            // the pass stops, so the next pass removes this copy and moves the file again
            // instead of leaving it twice.
            if (!file.delete() && file.exists()) {
                warn("a copied file could not be removed from the landing folder; the move is redone later", null)
                return MoveResult(MoveOutcome.FAILED, stopPass = true)
            }
            journal.clear()
            claims.release(target.path, file.path)
            if (placement.renumbered) {
                info("transfer ${transferId.take(8)}: a landed file reached Downloads under another name than promised")
            }
            return MoveResult(MoveOutcome.COPIED, placement.renumbered)
        } catch (e: Exception) {
            // A journal entry left by a failed copy points at a copy that was already removed
            // (or a pending MediaStore row); settling it before the next pass is harmless.
            warn("moving a landed file into Downloads failed; it stays in the landing folder", e)
            return MoveResult(MoveOutcome.FAILED)
        }
    }
}
