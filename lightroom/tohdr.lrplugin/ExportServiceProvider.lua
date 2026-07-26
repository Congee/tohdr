--[[----------------------------------------------------------------------

ExportServiceProvider.lua

The export service itself: takes the file Lightroom rendered for each photo,
runs `tohdr convert` on it, and puts the resulting gain-map HEIC where the
user asked for it.

Why a 16-bit TIFF intermediate is forced (see updateExportSettings): the whole
point of this plugin is to preserve above-SDR-white highlights long enough for
`tohdr` to derive a gain map from them. Lightroom's JPEG and 8-bit exports have
already clipped that data away, so letting the user pick one would produce a
file that passes every structural check while carrying no HDR at all -- the
exact failure docs/acceptance-criteria.md criterion 13 exists to catch.

------------------------------------------------------------------------]]

local LrDate = import 'LrDate'
local LrFileUtils = import 'LrFileUtils'
local LrPathUtils = import 'LrPathUtils'
local LrTasks = import 'LrTasks'

local TohdrCli = require 'TohdrCli'
local TohdrExportDialog = require 'TohdrExportDialog'

local exportServiceProvider = {}

-- Deliberately NOT setting `supportsIncrementalPublish`. Per the LrC 15.3 API
-- reference (API Reference/modules/SDK - Export service provider.html): "If not
-- present, this plug-in is available in Export only. When true, this plug-in can
-- be used for both Export and Publish. When set to the string 'only', the
-- plug-in is visible only in Publish."
--
-- This was `= 'only'`, copied from Adobe's own flickr.lrdevplugin sample, where
-- it is correct because Flickr is a publish destination. Here it made the plugin
-- invisible in File > Export -- the one place the README tells you to look --
-- while the whole design is export-shaped: processRenderedPhotos writes beside
-- rendition.destinationPath and there is not one publish callback in this file.
-- Absent is the right value; do not "helpfully" reinstate it.
exportServiceProvider.hideSections = { 'fileNaming', 'fileSettings', 'imageSettings' }
exportServiceProvider.allowFileFormats = { 'TIFF' }
exportServiceProvider.allowColorSpaces = nil
exportServiceProvider.canExportVideo = false

exportServiceProvider.exportPresetFields = {
	{ key = 'tohdr_flavor',           default = 'both' },
	{ key = 'tohdr_engine',           default = 'apple' },
	{ key = 'tohdr_maxSizeEnabled',   default = false },
	{ key = 'tohdr_maxSizeValue',     default = 4 },
	{ key = 'tohdr_maxSizeUnit',      default = 'MB' },
	{ key = 'tohdr_quality',          default = 85 },
	{ key = 'tohdr_minQuality',       default = 40 },
	{ key = 'tohdr_toneMap',          default = 'reinhard' },
	{ key = 'tohdr_gainSubsample',    default = 2 },
	{ key = 'tohdr_binaryPath',       default = '' },
}

exportServiceProvider.startDialog = TohdrExportDialog.startDialog
exportServiceProvider.sectionsForTopOfDialog = TohdrExportDialog.sectionsForTopOfDialog

--- Force the intermediate Lightroom renders to something `tohdr` can actually
--- read with headroom intact. See the file header for why this is not left to
--- the user.
---
--- Every key here was read out of Lightroom's own preferences after driving the
--- Export dialog by hand and confirming the resulting file carried a gain map:
--- LrC stores the dialog's state under an `AgExport_` prefix, and an export
--- service receives the same table under `LR_`. That was necessary because the
--- LrC 15.3 SDK Guide documents none of it -- the string "HDR" does not occur
--- anywhere in the guide, `LR_export_colorSpace` is documented as accepting
--- only sRGB/AdobeRGB/ProPhotoRGB, and `LR_export_bitDepth` as only 8 or 16.
--- The guide is simply behind the app; it also omits PNG, AVIF and JPEG XL,
--- which the dialog has offered for releases.
---
--- Three things here were wrong until the pref dump settled them:
---   * `LR_export_useHDR` was invented. No such key exists in LrC 15.4.1's
---     export module or in any documentation. It was silently ignored, so
---     every intermediate this plugin ever rendered was SDR.
---   * `ProPhotoRGB` is a wide-gamut *SDR* space, and `tohdr` read no ICC
---     profile on this path at the time -- it assumed sRGB primaries and a PQ
---     transfer for any 16-bit TIFF, so the pixels were misread twice over. It
---     reads the profile now, which is what allows the P3 request below.
---   * TIFF takes the bit depth from `bitDepthOthers`, not `bitDepth`; the
---     latter is the JPEG-era key. Both are set, since setting the wrong one
---     is silent.
function exportServiceProvider.updateExportSettings(exportSettings)
	exportSettings.LR_format = 'TIFF'
	-- The dialog's "HDR Output" checkbox. With it set, the colour-space values
	-- gain an `_hdr` suffix and Lightroom writes a *pair* of images into one
	-- TIFF: the SDR rendition in IFD0, and a full-resolution gain map in a
	-- SubIFD carrying ISO 21496-1 metadata. `tohdr` transcodes that pair
	-- directly rather than deriving anything.
	exportSettings.LR_enableHDRDisplay = true
	-- "HDR Display P3", not "HDR sRGB (Rec. 709)".
	--
	-- This was sRGB, for a reason that no longer holds: `tohdr` was BT.709 end to
	-- end and read no ICC profile on this path, so wider primaries would have been
	-- silently mislabelled rather than carried. It now reads IFD0's embedded
	-- profile and declares what it finds, so the wider export is the one to ask
	-- for -- and asking for the narrow one is not neutral. Measured on a real
	-- export of DSC07746 (`tohdr-apple/examples/probe_gamut.rs`), rendering into
	-- Rec.709 puts 12.33% of the frame outside the gamut it is being squeezed
	-- into, worst error dE 5.35, concentrated in a coherent yellow/yellow-green
	-- region rather than scattered. The sRGB export's own row is dE 0.00 for the
	-- least reassuring reason available: Lightroom had already clipped it, so
	-- there was nothing left to lose.
	--
	-- `p3_hdr` is LrC's own token, not a guess: it is the value LrC writes into
	-- `EditInPs_hdr_colorSpace` for its Edit-in-Photoshop HDR path, and the export
	-- keys take the same `<space>_hdr` spellings (this machine's prefs held
	-- `Rec2020_hdr`). None of it is documented. If a future LrC ignores the value,
	-- the intermediate arrives in some other space and `tohdr` reports the
	-- mismatch from the embedded profile instead of mislabelling the output --
	-- which is what makes asking for this safe rather than hopeful.
	exportSettings.LR_export_colorSpace = 'p3_hdr'
	exportSettings.LR_export_colorSpaceNonJPEG = 'p3_hdr'
	exportSettings.LR_export_bitDepth = 16
	exportSettings.LR_export_bitDepthOthers = 16
	-- Not 32: the float variant carries the identical gain map with the roles
	-- reversed (HDR base, downward map) at 1.5x the bytes, and `tohdr` wants
	-- the SDR base so it can ship Lightroom's own rendition.
	exportSettings.LR_tiff_compressionMethod = 'compressionMethod_None'
end

--- Locate the binary using the pure-Lua policy in TohdrCli, supplying it the
--- filesystem access it deliberately does not take itself.
---
--- Only two candidates: an explicit path the user set, and the binary bundled
--- inside this `.lrplugin`. No PATH search -- a `.lrplugin` is self-contained,
--- and Lightroom's sandbox could not read PATH to search it anyway.
local function findBinary(propertyTable)
	local pluginDir = _PLUGIN and _PLUGIN.path or nil
	local bundled = pluginDir and LrPathUtils.child(pluginDir, 'tohdr') or nil

	return TohdrCli.locateBinary {
		userBinaryPath = propertyTable.tohdr_binaryPath,
		pluginBinaryPath = bundled,
		fileExists = function(p)
			return p ~= nil and p ~= '' and LrFileUtils.exists(p) ~= false
		end,
	}
end

--- Run one command, capturing stdout+stderr into a temp file so a failure can
--- be reported with the CLI's own words instead of just an exit code.
---
--- The log name must not be predictable. Stock Lua's `math.random` without a
--- seed yields the same sequence on every fresh interpreter state, so the
--- first log path after a Lightroom relaunch was knowable in advance -- and
--- shell `>` follows symlinks, so anything pre-placed at that path in the
--- shared temp directory would receive whatever `tohdr` printed. Mixing in a
--- per-call counter and an address-derived value from a fresh table makes the
--- name unguessable without needing a seed source Lua does not portably have.
---
--- The clock comes from `LrDate.currentTime()`, not `os.time()`. Lightroom's
--- sandbox is demonstrably missing `os.getenv` (it crashed an export here), and
--- nothing documents which other `os` members survive -- so this file no longer
--- touches `os` at all. LrDate.currentTime returns seconds since 2001-01-01
--- UTC and may be fractional, hence the floor.
local logCounter = 0
local function tempLogPath()
	logCounter = logCounter + 1
	local entropy = tostring({}):gsub('%W', '')
	return LrPathUtils.child(
		LrPathUtils.getStandardFilePath('temp'),
		string.format('tohdr-%s-%d-%d.log',
			entropy, logCounter, math.floor(LrDate.currentTime()))
	)
end

local function runCapturing(commandLine)
	local logPath = tempLogPath()
	local full = commandLine .. ' >' .. TohdrCli.quoteArg(logPath) .. ' 2>&1'
	local status = LrTasks.execute(full)

	local output = ''
	if LrFileUtils.exists(logPath) then
		output = LrFileUtils.readFile(logPath) or ''
		LrFileUtils.delete(logPath)
	end
	return status, output
end

function exportServiceProvider.processRenderedPhotos(_functionContext, exportContext)
	local exportSession = exportContext.exportSession
	local propertyTable = exportContext.propertyTable
	local nPhotos = exportSession:countRenditions()

	local progress = exportContext:configureProgress {
		title = nPhotos > 1
			and ('Converting ' .. nPhotos .. ' photos to gain-map HEIC')
			or 'Converting one photo to gain-map HEIC',
	}

	local binary, locateErr = findBinary(propertyTable)

	for _, rendition in exportContext:renditions { stopIfCanceled = true } do
		-- Fail every rendition individually rather than aborting the export:
		-- Lightroom shows the message per photo, which is what the user needs
		-- to see when only some conversions fail.
		if not binary then
			rendition:uploadFailed(locateErr or 'tohdr binary not found')
		else
			local ok, pathOrMessage = rendition:waitForRender()
			if not ok then
				rendition:uploadFailed(pathOrMessage or 'Lightroom failed to render this photo')
			else
				local renderedPath = pathOrMessage
				local destDir = LrPathUtils.parent(rendition.destinationPath)
				local outName = LrPathUtils.removeExtension(
					LrPathUtils.leafName(rendition.destinationPath)
				) .. '.heic'
				local outPath = LrPathUtils.child(destDir, outName)

				local args = TohdrCli.buildConvertArgs(propertyTable, renderedPath, outPath)
				local cmd = TohdrCli.buildCommandLine(binary, args)
				local status, output = runCapturing(cmd)

				if status ~= 0 then
					rendition:uploadFailed(TohdrCli.summarizeFailure(status, output))
				elseif not LrFileUtils.exists(outPath) then
					rendition:uploadFailed(
						'tohdr reported success but wrote no file to ' .. outPath
					)
				elseif output:find('gain map: derived', 1, true) then
					-- The intermediate had no gain map in it, so `tohdr` fell
					-- back to deriving one from whatever pixels it found. On
					-- this path those pixels are Lightroom's SDR rendition,
					-- already clipped at diffuse white, and a gain map derived
					-- from them describes no HDR at all: the output would pass
					-- every structural check and still render washed out. That
					-- is the failure this whole project exists to prevent, so
					-- it is reported rather than shipped.
					--
					-- Two causes, and the user can tell them apart: the photo
					-- is not in HDR edit mode (Develop's HDR toggle off, so
					-- there is genuinely no headroom to carry), or this build
					-- of Lightroom did not honour the HDR export keys.
					LrFileUtils.delete(outPath)
					rendition:uploadFailed(
						'Lightroom rendered an SDR intermediate with no gain map, so the HEIC '
							.. 'would have carried no HDR. Check that this photo is in HDR edit '
							.. 'mode in Develop; if it is, this Lightroom version may not accept '
							.. 'the HDR export settings the plugin requests.'
					)
				end

				-- Delete the intermediate on EVERY path, not just success. It
				-- is a full-resolution uncompressed 16-bit TIFF -- hundreds of
				-- MB for a real photo -- and the user never asked for it, so
				-- leaving one behind per failed rendition fills the render
				-- cache silently.
				if renderedPath ~= outPath then
					LrFileUtils.delete(renderedPath)
				end
			end
		end
	end

	if progress then
		progress:done()
	end
end

return exportServiceProvider
