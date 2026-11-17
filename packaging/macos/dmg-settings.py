# How the DMG's window looks, for dmgbuild, which scripts/macos-package.sh
# runs with -D app=<OmniMem.app> and -D icon=<OmniMem.icns>.
#
# dmgbuild writes the window's .DS_Store itself rather than asking Finder
# to, so this works on a CI runner with nobody logged in. It finds
# dmg-background@2x.png beside the background and joins the two into one
# picture for Retina screens.
#
# Positions are in points from the window's top left and are where an icon's
# centre goes. They match dmg-background.svg: change both together.
import os.path

here = os.path.dirname(os.path.abspath(defines["settings"]))

format = "UDZO"
filesystem = "HFS+"

files = [defines["app"]]
symlinks = {"Applications": "/Applications"}
# The mounted volume's own icon, on the desktop and in Finder's sidebar.
icon = defines["icon"]

background = os.path.join(here, "dmg-background.png")
window_rect = ((200, 120), (660, 400))
default_view = "icon-view"
include_icon_view_settings = True
include_list_view_settings = False
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False

arrange_by = None
icon_size = 128
text_size = 13
label_pos = "bottom"
icon_locations = {
    "OmniMem.app": (180, 185),
    "Applications": (480, 185),
}
