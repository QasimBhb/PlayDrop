package org.playdrop.android.ui.common

import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow

/** Emits the current Unix time in seconds now and then every [periodMs]. */
fun nowTicker(periodMs: Long): Flow<Long> = flow {
    while (true) {
        emit(Format.nowSecs())
        delay(periodMs)
    }
}

/** Emits the current Unix time in milliseconds now and then every [periodMs]. */
fun nowMsTicker(periodMs: Long): Flow<Long> = flow {
    while (true) {
        emit(System.currentTimeMillis())
        delay(periodMs)
    }
}
