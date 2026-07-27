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

	-- The fourth field is the build. It *accepts* a string -- Adobe's own LrC 15.3
	-- samples all carry
	--   VERSION = { major=15, minor=3, revision=0, build="202604090947-8f3672ed" }
	-- a UTC timestamp and a git short hash, the same string that names the SDK
	-- bundle it shipped in -- but Plug-in Manager will not render a string as the
	-- fourth component of `0.1.0.x`, so two installs that differ only there look
	-- identical on screen. Measured, not assumed: two installs four hours apart
	-- carried different strings and showed no visible difference.
	--
	-- So this is a *number*, as Adobe's older samples have it (`build=200000`).
	-- `tools/install-lrplugin.sh` sets it to `YYMMDDHHMM` in UTC, which rises with
	-- every install and so always differs from the copy it replaced -- including a
	-- reinstall from a dirty tree, which is exactly when "is this my edit?" gets
	-- asked. The git hash keeps the precision, on the `installed-from` line below.
	--
	-- Worth using here, because this plugin has a provenance problem the same shape
	-- as Adobe's. The `Modules` folder is scanned only at launch and a running
	-- Lightroom holds the `.lua` files it loaded, so "is Lightroom running the code
	-- I just edited?" is a real question with no answer from inside the plugin --
	-- access times do not move on those files, which cost an hour of debugging
	-- once. Plug-in Manager shows this version string, so stamping the commit into
	-- it answers that question by looking.
	--
	-- `build = 0` is what a checkout says, so a stamp can never be committed by
	-- accident. **Bump `revision` on every change that ships**, so the version
	-- reads as a change and not only as a reinstall.
	VERSION = { major = 0, minor = 1, revision = 0, build = 0 },
	-- installed-from: checkout

	LrExportServiceProvider = {
		title = "HDR Gain-Map HEIC",
		file = 'ExportServiceProvider.lua',
	},
}
