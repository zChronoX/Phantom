package app.phantom.host.core

import kotlin.math.abs

/**
 * Istante di cattura dei pacchetti audio ricavato dal **numero di campioni letti**, non dall'ora in cui `read()`
 * ritorna. L'ora di ritorno di `read()` balla di qualche millisecondo (scheduler, carico dell'encoder video) e
 * l'iPhone usa questi istanti per mettere ogni pacchetto al suo posto: con l'ora di `read()` i pacchetti si
 * sovrapponevano o lasciavano buchi, e l'audio gracchiava.
 *
 * Se l'orologio dei campioni si allontana troppo dall'orologio reale (la cattura si è fermata, l'`AudioRecord` ha
 * perso dati) si riparte dall'ora attuale: l'iPhone vede un salto e lo tratta come un buco.
 */
class AudioSampleClock(
    private val sampleRate: Int,
    private val maxDriftUs: Long = 200_000L,
) {
    private var baseUs = -1L
    private var samples = 0L

    /** Quante volte l'orologio è ripartito dall'ora attuale. */
    var rebases = 0
        private set

    /** È stato appena letto un blocco di `chunkSamples` campioni: restituisce l'istante del suo primo campione (µs). */
    fun onChunk(chunkSamples: Int, nowUs: Long): Long {
        val chunkUs = chunkSamples * 1_000_000L / sampleRate
        if (baseUs < 0L) {
            baseUs = nowUs - chunkUs
            samples = 0L
        }
        var pts = baseUs + samples * 1_000_000L / sampleRate
        if (abs(nowUs - (pts + chunkUs)) > maxDriftUs) {
            baseUs = nowUs - chunkUs
            samples = 0L
            pts = baseUs
            rebases++
        }
        samples += chunkSamples
        return pts
    }
}
