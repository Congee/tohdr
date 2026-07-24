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

	VERSION = { major = 0, minor = 1, revision = 0, build = 0 },

	LrExportServiceProvider = {
		title = "HDR Gain-Map HEIC",
		file = 'ExportServiceProvider.lua',
	},
}
