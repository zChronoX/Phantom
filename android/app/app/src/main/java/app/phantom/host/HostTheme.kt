package app.phantom.host

import android.content.Context
import android.content.res.Configuration
import android.graphics.Color
import android.graphics.drawable.GradientDrawable

/**
 * Palette monocromatica adattiva (Light / Dark) di Phantom Host.
 * Zero librerie Material/Compose: soli drawable nativi leggeri (ADR-010 / ADR-011).
 */
internal data class HostTheme(
    val isDark: Boolean,
    val canvas: Int,
    val card: Int,
    val elevated: Int,
    val hairline: Int,
    val textPrimary: Int,
    val textSecondary: Int,
    val ctaFill: Int,
    val ctaText: Int,
    val liveGreen: Int,
    val warnAmber: Int,
    val dangerRed: Int,
    val iconPlateFill: Int,
) {
    fun cardDrawable(dp: Float, radiusDp: Float = 24f): GradientDrawable = GradientDrawable().apply {
        setColor(card)
        cornerRadius = radiusDp * dp
        setStroke(strokePx(dp), hairline)
    }

    fun elevatedDrawable(dp: Float, radiusDp: Float = 16f): GradientDrawable = GradientDrawable().apply {
        setColor(elevated)
        cornerRadius = radiusDp * dp
        setStroke(strokePx(dp), hairline)
    }

    fun capsuleDrawable(fillColor: Int, strokeColor: Int? = null, dp: Float): GradientDrawable =
        GradientDrawable().apply {
            setColor(fillColor)
            cornerRadius = 999f * dp
            if (strokeColor != null) {
                setStroke(strokePx(dp), strokeColor)
            }
        }

    fun segmentDrawable(selected: Boolean, dp: Float): GradientDrawable = GradientDrawable().apply {
        setColor(if (selected) ctaFill else Color.TRANSPARENT)
        cornerRadius = 999f * dp
    }

    fun iconPlateDrawable(dp: Float): GradientDrawable = GradientDrawable().apply {
        setColor(iconPlateFill)
        cornerRadius = 11f * dp
        if (isDark) {
            setStroke(strokePx(dp), hairline)
        }
    }

    fun dotDrawable(color: Int): GradientDrawable = GradientDrawable().apply {
        shape = GradientDrawable.OVAL
        setColor(color)
    }

    private fun strokePx(dp: Float): Int = (0.75f * dp).toInt().coerceAtLeast(1)

    companion object {
        fun from(context: Context): HostTheme {
            val nightMask = context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK
            val dark = nightMask == Configuration.UI_MODE_NIGHT_YES
            return if (dark) {
                HostTheme(
                    isDark = true,
                    canvas = Color.parseColor("#000000"),
                    card = Color.parseColor("#121214"),
                    elevated = Color.parseColor("#1C1C1E"),
                    hairline = Color.parseColor("#24FFFFFF"),
                    textPrimary = Color.parseColor("#F5F5F7"),
                    textSecondary = Color.parseColor("#8E8E93"),
                    ctaFill = Color.parseColor("#FFFFFF"),
                    ctaText = Color.parseColor("#0A0A0C"),
                    liveGreen = Color.parseColor("#30D158"),
                    warnAmber = Color.parseColor("#FF9F0A"),
                    dangerRed = Color.parseColor("#FF453A"),
                    iconPlateFill = Color.parseColor("#0A0A0C"),
                )
            } else {
                HostTheme(
                    isDark = false,
                    canvas = Color.parseColor("#F5F5F7"),
                    card = Color.parseColor("#FFFFFF"),
                    elevated = Color.parseColor("#EBEBF0"),
                    hairline = Color.parseColor("#14000000"),
                    textPrimary = Color.parseColor("#0A0A0C"),
                    textSecondary = Color.parseColor("#6E6E73"),
                    ctaFill = Color.parseColor("#0A0A0C"),
                    ctaText = Color.parseColor("#FFFFFF"),
                    liveGreen = Color.parseColor("#16A34A"),
                    warnAmber = Color.parseColor("#D97706"),
                    dangerRed = Color.parseColor("#DC2626"),
                    iconPlateFill = Color.parseColor("#0A0A0C"),
                )
            }
        }
    }
}
