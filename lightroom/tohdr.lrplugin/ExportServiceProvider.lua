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

local LrFileUtils = import 'LrFileUtils'
local LrPathUtils = import 'LrPathUtils'
local LrTasks = import 'LrTasks'

local TohdrCli = require 'TohdrCli'
local TohdrExportDialog = require 'TohdrExportDialog'

local exportServiceProvider = {}

exportServiceProvider.supportsIncrementalPublish = 'only'
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
function exportServiceProvider.updateExportSettings(exportSettings)
	exportSettings.LR_format = 'TIFF'
	exportSettings.LR_export_bitDepth = 16
	exportSettings.LR_export_colorSpace = 'ProPhotoRGB'
	exportSettings.LR_tiff_compressionMethod = 'compressionMethod_None'
	-- Ask Lightroom for HDR pixels rather than a tone-mapped SDR rendition.
	-- Only meaningful for photos in HDR edit mode; harmless otherwise.
	exportSettings.LR_export_useHDR = true
end

--- Locate the binary using the pure-Lua policy in TohdrCli, supplying it the
--- filesystem and environment access it deliberately does not take itself.
local function findBinary(propertyTable)
	local pluginDir = _PLUGIN and _PLUGIN.path or nil
	local bundled = pluginDir and LrPathUtils.child(pluginDir, 'tohdr') or nil

	return TohdrCli.locateBinary {
		userBinaryPath = propertyTable.tohdr_binaryPath,
		pluginBinaryPath = bundled,
		pathDirs = TohdrCli.splitPath(TohdrCli.defaultPathEnv()),
		fileExists = function(p)
			return p ~= nil and p ~= '' and LrFileUtils.exists(p) ~= false
		end,
		joinPath = function(dir, name)
			return LrPathUtils.child(dir, name)
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
local logCounter = 0
local function tempLogPath()
	logCounter = logCounter + 1
	local entropy = tostring({}):gsub('%W', '')
	return LrPathUtils.child(
		LrPathUtils.getStandardFilePath('temp'),
		string.format('tohdr-%s-%d-%d.log', entropy, logCounter, os.time())
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
