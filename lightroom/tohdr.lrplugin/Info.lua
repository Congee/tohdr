--[[----------------------------------------------------------------------

Info.lua
Plugin manifest for the "HDR Gain-Map HEIC" Lightroom Classic export service.

------------------------------------------------------------------------]]

return {

	-- LrSdkVersion / LrSdkMinimumVersion = 13.0.
	--
	-- Why 13.0 and not something older: this plugin exists to hand Lightroom's
	-- rendered pixels to `tohdr`, and the *only* reason that is useful is if
	-- those pixels still carry above-SDR-white highlight data. Lightroom
	-- Classic's "HDR Output" export mode (File Settings > Enable HDR Output)
	-- and HDR editing (Develop > Basics > HDR) were introduced in Lightroom
	-- Classic 13.0 (Oct 2023) -- see Adobe's own HDR documentation:
	--   https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/hdr-output.html
	-- and the LrC 13.0 SDK changelog (new LrPhoto:getDevelopSettings() fields
	-- HDREditMode / HDRMaxValue / SDR*, new AVIF/JXL format support):
	--   https://community.adobe.com/t5/lightroom-classic-discussions/p-changes-to-the-lr-13-0-sdk/m-p/14150918
	-- None of the Lua APIs this plugin calls (LrExportSession, LrTasks,
	-- LrView, LrDialogs, LrFileUtils, LrPathUtils, LrProgressScope) are new
	-- in 13.0 -- they have been stable since much older SDKs. Declaring
	-- 13.0 here is a *product* decision, not an API dependency: it keeps
	-- the plugin from silently loading (and producing washed-out,
	-- SDR-only "gain maps") on a Lightroom Classic version that has no
	-- HDR Output mode to feed it in the first place.
	LrSdkVersion = 13.0,
	LrSdkMinimumVersion = 13.0,

	LrToolkitIdentifier = 'com.tohdr.lightroom-export',
	LrPluginName = "HDR Gain-Map HEIC",

	-- The fourth field is the build, and it takes a *string* as well as a number.
	-- Adobe's own LrC 15.3 samples all carry
	--   VERSION = { major=15, minor=3, revision=0, build="202604090947-8f3672ed" }
	-- which is a UTC build timestamp and a git short hash -- the same string that
	-- names the SDK bundle it shipped in. (Its older samples use `build=200000`,
	-- an integer, so both forms work. The SDK Guide documents neither; this is read
	-- off `docs/LrC_15.3_*/Sample Plugins/*/Info.lua`.)
	--
	-- Worth using here, because this plugin has a provenance problem the same shape
	-- as Adobe's. The `Modules` folder is scanned only at launch and a running
	-- Lightroom holds the `.lua` files it loaded, so "is Lightroom running the code
	-- I just edited?" is a real question with no answer from inside the plugin --
	-- access times do not move on those files, which cost an hour of debugging
	-- once. Plug-in Manager shows this version string, so stamping the commit into
	-- it answers that question by looking.
	--
	-- `"dev"` is what a checkout says. `tools/install-lrplugin.sh` rewrites this
	-- line as it copies the bundle, so an installed plugin names its own commit.
	VERSION = { major = 0, minor = 1, revision = 0, build = "dev" },

	LrExportServiceProvider = {
		title = "HDR Gain-Map HEIC",
		file = 'ExportServiceProvider.lua',
	},
}
