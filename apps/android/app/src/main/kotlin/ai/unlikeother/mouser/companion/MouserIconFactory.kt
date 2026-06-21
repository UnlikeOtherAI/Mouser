package ai.unlikeother.mouser.companion

import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.PathBuilder
import androidx.compose.ui.graphics.vector.path
import androidx.compose.ui.unit.dp

/**
 * Shared builders for the local Material-style vector glyphs ([MouserIcons] and
 * [ClipboardIcons]).
 *
 * Split out so the icon-definition files each stay small and cohesive while we
 * avoid `material-icons-extended` (audit R2 LOW: release bloat). Both helpers are
 * `internal` so the two icon objects can reuse them without re-exporting the
 * pattern app-wide.
 */

/**
 * Builds (once) a 24dp×24dp Material-style [ImageVector] with a single tintable
 * path group, mirroring the shape of `androidx.compose.material.icons`' own
 * `materialIcon` factory so call-sites read identically (`MouserIcons.Foo`).
 */
@Composable
internal fun materialIcon(
    name: String,
    block: ImageVector.Builder.() -> Unit
): ImageVector = remember(name) {
    ImageVector.Builder(
        name = "mouser.$name",
        defaultWidth = 24.dp,
        defaultHeight = 24.dp,
        viewportWidth = 24f,
        viewportHeight = 24f
    ).apply(block).build()
}

/** A solid-fill path that inherits the icon's tint, like the Material factory. */
internal inline fun ImageVector.Builder.materialPath(
    pathBuilder: PathBuilder.() -> Unit
) {
    // SolidColor(Color.Black) is the tintable sentinel the Material icons use; the
    // Icon composable overrides it with `LocalContentColor`/the supplied `tint`.
    path(fill = SolidColor(Color.Black), pathBuilder = pathBuilder)
}
