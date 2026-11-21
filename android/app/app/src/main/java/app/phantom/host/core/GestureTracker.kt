package app.phantom.host.core

/** Un punto di un tratto: coordinate in pixel dello schermo e istante (ms). */
data class GesturePoint(val x: Float, val y: Float, val tMs: Long)

/** Il percorso di un dito, dal momento in cui si appoggia a quando si alza. */
data class GestureStroke(val points: List<GesturePoint>) {
    val startMs: Long get() = points.first().tMs
    val durationMs: Long get() = points.last().tMs - points.first().tMs
}

/**
 * Ricostruisce i gesti completi dagli stati del tocco dell'iPhone, per il controllo tramite Accessibilità (modalità
 * Standard, senza root né Shizuku). L'Accessibilità non sa iniettare eventi dito per dito come la modalità Root:
 * accetta solo gesti interi (`dispatchGesture`). Quindi si raccolgono i percorsi di tutte le dita e il gesto parte
 * quando l'ultima si alza: tap, pressione lunga, swipe, trascinamento e pinch a due dita funzionano, ma con il ritardo
 * della durata del gesto stesso.
 */
class GestureTracker(
    private val maxStrokes: Int = 10,
    private val maxPointsPerStroke: Int = 60,
    private val minSampleSpacingPx: Float = 4f,
) {
    private val active = LinkedHashMap<Int, MutableList<GesturePoint>>()
    private val finished = ArrayList<GestureStroke>()

    fun reset() {
        active.clear()
        finished.clear()
    }

    /**
     * Nuovo stato completo delle dita (coordinate normalizzate 0–65535). Restituisce il gesto completo (un tratto per
     * dito) quando tutte le dita si sono alzate, altrimenti `null`.
     */
    fun update(fingers: List<NormalizedInputFinger>, width: Int, height: Int, nowMs: Long): List<GestureStroke>? {
        val maxX = (width - 1).coerceAtLeast(1).toFloat()
        val maxY = (height - 1).coerceAtLeast(1).toFloat()
        val present = HashSet<Int>()
        for (f in fingers) {
            present.add(f.id)
            val x = f.x.coerceIn(0, 65_535) / 65_535f * maxX
            val y = f.y.coerceIn(0, 65_535) / 65_535f * maxY
            val stroke = active[f.id]
            if (stroke == null) {
                if (active.size + finished.size >= maxStrokes) continue
                active[f.id] = mutableListOf(GesturePoint(x, y, nowMs))
            } else {
                val last = stroke.last()
                val dx = x - last.x
                val dy = y - last.y
                if (dx * dx + dy * dy >= minSampleSpacingPx * minSampleSpacingPx) {
                    stroke.add(GesturePoint(x, y, nowMs))
                }
            }
        }
        // dita alzate: il loro tratto finisce adesso (l'ultimo punto porta l'istante di fine)
        val lifted = active.keys.filter { it !in present }
        for (id in lifted) {
            val stroke = active.remove(id) ?: continue
            val last = stroke.last()
            if (last.tMs != nowMs) stroke.add(GesturePoint(last.x, last.y, nowMs))
            finished.add(GestureStroke(downsample(stroke)))
        }
        if (active.isNotEmpty() || finished.isEmpty()) return null
        val gesture = ArrayList(finished)
        finished.clear()
        return gesture
    }

    private fun downsample(points: List<GesturePoint>): List<GesturePoint> {
        if (points.size <= maxPointsPerStroke) return points
        val out = ArrayList<GesturePoint>(maxPointsPerStroke)
        val step = (points.size - 1).toDouble() / (maxPointsPerStroke - 1)
        for (i in 0 until maxPointsPerStroke) {
            out.add(points[(i * step).toInt().coerceAtMost(points.size - 1)])
        }
        out[out.size - 1] = points.last()
        return out
    }
}
