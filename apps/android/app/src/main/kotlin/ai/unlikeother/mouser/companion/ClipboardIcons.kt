package ai.unlikeother.mouser.companion

import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.vector.ImageVector

/**
 * Local Material-style vector glyphs for the **clipboard** screen.
 *
 * Same rationale as [MouserIcons] — we avoid `material-icons-extended` (audit R2
 * LOW: release bloat) and re-author each needed 24dp glyph here via the shared
 * [materialIcon] factory (MouserIconFactory.kt). Kept separate from the
 * touchpad/device glyphs so each file stays small and cohesive.
 */
object ClipboardIcons {

    /** Copy / "from this device" glyph (clipboard transfer). */
    val ContentCopy: ImageVector
        @Composable get() = materialIcon("ContentCopy") {
            materialPath {
                moveTo(16.0f, 1.0f)
                horizontalLineTo(4.0f)
                curveTo(2.9f, 1.0f, 2.0f, 1.9f, 2.0f, 3.0f)
                verticalLineToRelative(14.0f)
                horizontalLineToRelative(2.0f)
                verticalLineTo(3.0f)
                horizontalLineToRelative(12.0f)
                verticalLineTo(1.0f)
                close()
                moveTo(19.0f, 5.0f)
                horizontalLineTo(8.0f)
                curveTo(6.9f, 5.0f, 6.0f, 5.9f, 6.0f, 7.0f)
                verticalLineToRelative(14.0f)
                curveToRelative(0.0f, 1.1f, 0.9f, 2.0f, 2.0f, 2.0f)
                horizontalLineToRelative(11.0f)
                curveToRelative(1.1f, 0.0f, 2.0f, -0.9f, 2.0f, -2.0f)
                verticalLineTo(7.0f)
                curveToRelative(0.0f, -1.1f, -0.9f, -2.0f, -2.0f, -2.0f)
                close()
                moveTo(19.0f, 21.0f)
                horizontalLineTo(8.0f)
                verticalLineTo(7.0f)
                horizontalLineToRelative(11.0f)
                verticalLineToRelative(14.0f)
                close()
            }
        }

    /** Paste / "to this device" glyph (clipboard transfer). */
    val ContentPaste: ImageVector
        @Composable get() = materialIcon("ContentPaste") {
            materialPath {
                moveTo(19.0f, 2.0f)
                horizontalLineToRelative(-4.18f)
                curveTo(14.4f, 0.84f, 13.3f, 0.0f, 12.0f, 0.0f)
                reflectiveCurveToRelative(-2.4f, 0.84f, -2.82f, 2.0f)
                horizontalLineTo(5.0f)
                curveToRelative(-1.1f, 0.0f, -2.0f, 0.9f, -2.0f, 2.0f)
                verticalLineToRelative(16.0f)
                curveToRelative(0.0f, 1.1f, 0.9f, 2.0f, 2.0f, 2.0f)
                horizontalLineToRelative(14.0f)
                curveToRelative(1.1f, 0.0f, 2.0f, -0.9f, 2.0f, -2.0f)
                verticalLineTo(4.0f)
                curveToRelative(0.0f, -1.1f, -0.9f, -2.0f, -2.0f, -2.0f)
                close()
                moveTo(12.0f, 2.0f)
                curveToRelative(0.55f, 0.0f, 1.0f, 0.45f, 1.0f, 1.0f)
                reflectiveCurveToRelative(-0.45f, 1.0f, -1.0f, 1.0f)
                reflectiveCurveToRelative(-1.0f, -0.45f, -1.0f, -1.0f)
                reflectiveCurveToRelative(0.45f, -1.0f, 1.0f, -1.0f)
                close()
                moveTo(19.0f, 20.0f)
                horizontalLineTo(5.0f)
                verticalLineTo(4.0f)
                horizontalLineToRelative(2.0f)
                verticalLineToRelative(3.0f)
                horizontalLineToRelative(10.0f)
                verticalLineTo(4.0f)
                horizontalLineToRelative(2.0f)
                verticalLineToRelative(16.0f)
                close()
            }
        }

    /** Sync / circular-arrows glyph (master clipboard toggle, transfer in-flight). */
    val Sync: ImageVector
        @Composable get() = materialIcon("Sync") {
            materialPath {
                moveTo(12.0f, 4.0f)
                verticalLineTo(1.0f)
                lineTo(8.0f, 5.0f)
                lineToRelative(4.0f, 4.0f)
                verticalLineTo(6.0f)
                curveToRelative(3.31f, 0.0f, 6.0f, 2.69f, 6.0f, 6.0f)
                curveToRelative(0.0f, 1.01f, -0.25f, 1.97f, -0.7f, 2.8f)
                lineToRelative(1.46f, 1.46f)
                curveTo(19.54f, 15.03f, 20.0f, 13.57f, 20.0f, 12.0f)
                curveToRelative(0.0f, -4.42f, -3.58f, -8.0f, -8.0f, -8.0f)
                close()
                moveTo(12.0f, 18.0f)
                curveToRelative(-3.31f, 0.0f, -6.0f, -2.69f, -6.0f, -6.0f)
                curveToRelative(0.0f, -1.01f, 0.25f, -1.97f, 0.7f, -2.8f)
                lineTo(5.24f, 7.74f)
                curveTo(4.46f, 8.97f, 4.0f, 10.43f, 4.0f, 12.0f)
                curveToRelative(0.0f, 4.42f, 3.58f, 8.0f, 8.0f, 8.0f)
                verticalLineToRelative(3.0f)
                lineToRelative(4.0f, -4.0f)
                lineToRelative(-4.0f, -4.0f)
                verticalLineToRelative(3.0f)
                close()
            }
        }

    /** Image/photo glyph (sync-images toggle). */
    val Image: ImageVector
        @Composable get() = materialIcon("Image") {
            materialPath {
                moveTo(21.0f, 19.0f)
                verticalLineTo(5.0f)
                curveToRelative(0.0f, -1.1f, -0.9f, -2.0f, -2.0f, -2.0f)
                horizontalLineTo(5.0f)
                curveToRelative(-1.1f, 0.0f, -2.0f, 0.9f, -2.0f, 2.0f)
                verticalLineToRelative(14.0f)
                curveToRelative(0.0f, 1.1f, 0.9f, 2.0f, 2.0f, 2.0f)
                horizontalLineToRelative(14.0f)
                curveToRelative(1.1f, 0.0f, 2.0f, -0.9f, 2.0f, -2.0f)
                close()
                moveTo(8.5f, 13.5f)
                lineToRelative(2.5f, 3.01f)
                lineTo(14.5f, 12.0f)
                lineToRelative(4.5f, 6.0f)
                horizontalLineTo(5.0f)
                lineToRelative(3.5f, -4.5f)
                close()
            }
        }

    /** File/document glyph (sync-files toggle). */
    val InsertDriveFile: ImageVector
        @Composable get() = materialIcon("InsertDriveFile") {
            materialPath {
                moveTo(6.0f, 2.0f)
                curveToRelative(-1.1f, 0.0f, -1.99f, 0.9f, -1.99f, 2.0f)
                lineTo(4.0f, 20.0f)
                curveToRelative(0.0f, 1.1f, 0.89f, 2.0f, 1.99f, 2.0f)
                horizontalLineTo(18.0f)
                curveToRelative(1.1f, 0.0f, 2.0f, -0.9f, 2.0f, -2.0f)
                verticalLineTo(8.0f)
                lineToRelative(-6.0f, -6.0f)
                horizontalLineTo(6.0f)
                close()
                moveTo(13.0f, 9.0f)
                verticalLineTo(3.5f)
                lineTo(18.5f, 9.0f)
                horizontalLineTo(13.0f)
                close()
            }
        }

    /** "Aa" text-fields glyph (sync-text toggle). */
    val TextFields: ImageVector
        @Composable get() = materialIcon("TextFields") {
            materialPath {
                moveTo(2.5f, 4.0f)
                verticalLineToRelative(3.0f)
                horizontalLineToRelative(5.0f)
                verticalLineToRelative(12.0f)
                horizontalLineToRelative(3.0f)
                verticalLineTo(7.0f)
                horizontalLineToRelative(5.0f)
                verticalLineTo(4.0f)
                horizontalLineToRelative(-13.0f)
                close()
                moveTo(21.5f, 9.0f)
                horizontalLineToRelative(-9.0f)
                verticalLineToRelative(3.0f)
                horizontalLineToRelative(3.0f)
                verticalLineToRelative(7.0f)
                horizontalLineToRelative(3.0f)
                verticalLineToRelative(-7.0f)
                horizontalLineToRelative(3.0f)
                verticalLineTo(9.0f)
                close()
            }
        }

    /** Sliders/tune glyph (clipboard settings header). */
    val Tune: ImageVector
        @Composable get() = materialIcon("Tune") {
            materialPath {
                moveTo(3.0f, 17.0f)
                verticalLineToRelative(2.0f)
                horizontalLineToRelative(6.0f)
                verticalLineToRelative(-2.0f)
                horizontalLineTo(3.0f)
                close()
                moveTo(3.0f, 5.0f)
                verticalLineToRelative(2.0f)
                horizontalLineToRelative(10.0f)
                verticalLineTo(5.0f)
                horizontalLineTo(3.0f)
                close()
                moveTo(13.0f, 21.0f)
                verticalLineToRelative(-2.0f)
                horizontalLineToRelative(8.0f)
                verticalLineToRelative(-2.0f)
                horizontalLineToRelative(-8.0f)
                verticalLineToRelative(-2.0f)
                horizontalLineToRelative(-2.0f)
                verticalLineToRelative(6.0f)
                horizontalLineToRelative(2.0f)
                close()
                moveTo(7.0f, 9.0f)
                verticalLineToRelative(2.0f)
                horizontalLineTo(3.0f)
                verticalLineToRelative(2.0f)
                horizontalLineToRelative(4.0f)
                verticalLineToRelative(2.0f)
                horizontalLineToRelative(2.0f)
                verticalLineTo(9.0f)
                horizontalLineTo(7.0f)
                close()
                moveTo(21.0f, 13.0f)
                verticalLineToRelative(-2.0f)
                horizontalLineToRelative(-10.0f)
                verticalLineToRelative(2.0f)
                horizontalLineToRelative(10.0f)
                close()
                moveTo(15.0f, 9.0f)
                horizontalLineToRelative(2.0f)
                verticalLineTo(7.0f)
                horizontalLineToRelative(4.0f)
                verticalLineTo(5.0f)
                horizontalLineToRelative(-4.0f)
                verticalLineTo(3.0f)
                horizontalLineToRelative(-2.0f)
                verticalLineToRelative(6.0f)
                close()
            }
        }

    /** Two-way arrows glyph (sync-direction control). */
    val SwapHoriz: ImageVector
        @Composable get() = materialIcon("SwapHoriz") {
            materialPath {
                moveTo(6.99f, 11.0f)
                lineTo(3.0f, 15.0f)
                lineToRelative(3.99f, 4.0f)
                verticalLineToRelative(-3.0f)
                horizontalLineTo(14.0f)
                verticalLineToRelative(-2.0f)
                horizontalLineTo(6.99f)
                verticalLineToRelative(-3.0f)
                close()
                moveTo(21.0f, 9.0f)
                lineToRelative(-3.99f, -4.0f)
                verticalLineToRelative(3.0f)
                horizontalLineTo(10.0f)
                verticalLineToRelative(2.0f)
                horizontalLineToRelative(7.01f)
                verticalLineToRelative(3.0f)
                lineTo(21.0f, 9.0f)
                close()
            }
        }

    /** Single right arrow (send-only direction). */
    val ArrowForward: ImageVector
        @Composable get() = materialIcon("ArrowForward") {
            materialPath {
                moveTo(12.0f, 4.0f)
                lineToRelative(-1.41f, 1.41f)
                lineTo(16.17f, 11.0f)
                horizontalLineTo(4.0f)
                verticalLineToRelative(2.0f)
                horizontalLineToRelative(12.17f)
                lineToRelative(-5.58f, 5.59f)
                lineTo(12.0f, 20.0f)
                lineToRelative(8.0f, -8.0f)
                close()
            }
        }

    /** Single left arrow (receive-only direction). */
    val ArrowBack: ImageVector
        @Composable get() = materialIcon("ArrowBack") {
            materialPath {
                moveTo(20.0f, 11.0f)
                horizontalLineTo(7.83f)
                lineToRelative(5.59f, -5.59f)
                lineTo(12.0f, 4.0f)
                lineToRelative(-8.0f, 8.0f)
                lineToRelative(8.0f, 8.0f)
                lineToRelative(1.41f, -1.41f)
                lineTo(7.83f, 13.0f)
                horizontalLineTo(20.0f)
                verticalLineToRelative(-2.0f)
                close()
            }
        }

    /** Apple-logo-free "devices" glyph (prefer-native toggle). */
    val Devices: ImageVector
        @Composable get() = materialIcon("Devices") {
            materialPath {
                moveTo(4.0f, 6.0f)
                horizontalLineToRelative(18.0f)
                verticalLineTo(4.0f)
                horizontalLineTo(4.0f)
                curveTo(2.9f, 4.0f, 2.0f, 4.9f, 2.0f, 6.0f)
                verticalLineToRelative(11.0f)
                horizontalLineTo(0.0f)
                verticalLineToRelative(3.0f)
                horizontalLineToRelative(14.0f)
                verticalLineToRelative(-3.0f)
                horizontalLineTo(4.0f)
                verticalLineTo(6.0f)
                close()
                moveTo(23.0f, 8.0f)
                horizontalLineToRelative(-6.0f)
                curveToRelative(-0.55f, 0.0f, -1.0f, 0.45f, -1.0f, 1.0f)
                verticalLineToRelative(10.0f)
                curveToRelative(0.0f, 0.55f, 0.45f, 1.0f, 1.0f, 1.0f)
                horizontalLineToRelative(6.0f)
                curveToRelative(0.55f, 0.0f, 1.0f, -0.45f, 1.0f, -1.0f)
                verticalLineTo(9.0f)
                curveToRelative(0.0f, -0.55f, -0.45f, -1.0f, -1.0f, -1.0f)
                close()
                moveTo(22.0f, 17.0f)
                horizontalLineToRelative(-4.0f)
                verticalLineToRelative(-7.0f)
                horizontalLineToRelative(4.0f)
                verticalLineToRelative(7.0f)
                close()
            }
        }
}
