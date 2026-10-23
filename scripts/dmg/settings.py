# dmgbuild settings for the Annex disk image (used by scripts/package-macos.sh).
#
#   dmgbuild -s scripts/dmg/settings.py -D app=<Annex.app> -D background=<background.tiff> \
#            "Annex <version>" <out.dmg>
#
# Just the app and Applications: the uninstaller lives inside the app (About → Uninstall).
#
# Positions are icon centres in points and must match scripts/dmg/render-background.swift.
import os.path

app = defines["app"]  # noqa: F821 (dmgbuild provides `defines`)
background = defines["background"]  # noqa: F821

format = "UDZO"
filesystem = "HFS+"
files = [app]
symlinks = {"Applications": "/Applications"}
icon_locations = {
    os.path.basename(app): (180, 170),
    "Applications": (480, 170),
}

window_rect = ((200, 160), (660, 360))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False
include_icon_view_settings = True
icon_size = 112
text_size = 13
arrange_by = None
grid_spacing = 100
label_pos = "bottom"
