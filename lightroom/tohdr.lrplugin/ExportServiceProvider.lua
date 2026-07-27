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
local LrDialogs = import 'LrDialogs'
local LrFileUtils = import 'LrFileUtils'
local LrPathUtils = import 'LrPathUtils'
local LrTasks = import 'LrTasks'

local TohdrCli = require 'TohdrCli'
local TohdrExportDialog = require 'TohdrExportDialog'

local export_service_provider = {}

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
export_service_provider.hideSections = { 'fileNaming', 'fileSettings', 'imageSettings' }
export_service_provider.allowFileFormats = { 'TIFF' }
export_service_provider.allowColorSpaces = nil
export_service_provider.canExportVideo = false

export_service_provider.exportPresetFields = {
	{ key = 'tohdr_flavor',           default = 'both' },
	{ key = 'tohdr_engine',           default = 'apple' },
	{ key = 'tohdr_maxSizeEnabled',   default = false },
	{ key = 'tohdr_maxSizeValue',     default = 4 },
	{ key = 'tohdr_maxSizeUnit',      default = 'MB' },
	{ key = 'tohdr_quality',          default = 85 },
	{ key = 'tohdr_minQuality',       default = 40 },
	{ key = 'tohdr_toneMap',          default = 'reinhard' },
	{ key = 'tohdr_gainSubsample',    default = 2 },
	{ key = 'tohdr_makerNote',        default = true },
	{ key = 'tohdr_binaryPath',       default = '' },
}

-- Field names on the left are Adobe's contract and stay as Adobe spells them;
-- the functions on the right are ours. That is the naming rule this plugin
-- follows throughout: snake_case for everything we name, camelCase only where
-- Lightroom reads the name or hands us the object.
export_service_provider.startDialog = TohdrExportDialog.start_dialog
export_service_provider.sectionsForTopOfDialog = TohdrExportDialog.sections_for_top_of_dialog

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
function export_service_provider.updateExportSettings(export_settings)
	export_settings.LR_format = 'TIFF'
	-- The dialog's "HDR Output" checkbox. With it set, the colour-space values
	-- gain an `_hdr` suffix and Lightroom writes a *pair* of images into one
	-- TIFF: the SDR rendition in IFD0, and a full-resolution gain map in a
	-- SubIFD carrying ISO 21496-1 metadata. `tohdr` transcodes that pair
	-- directly rather than deriving anything.
	export_settings.LR_enableHDRDisplay = true
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
	export_settings.LR_export_colorSpace = 'p3_hdr'
	export_settings.LR_export_colorSpaceNonJPEG = 'p3_hdr'
	export_settings.LR_export_bitDepth = 16
	export_settings.LR_export_bitDepthOthers = 16
	-- Not 32: the float variant carries the identical gain map with the roles
	-- reversed (HDR base, downward map) at 1.5x the bytes, and `tohdr` wants
	-- the SDR base so it can ship Lightroom's own rendition.
	export_settings.LR_tiff_compressionMethod = 'compressionMethod_None'
end

--- Locate the binary using the pure-Lua policy in TohdrCli, supplying it the
--- filesystem access it deliberately does not take itself.
---
--- Only two candidates: an explicit path the user set, and the binary bundled
--- inside this `.lrplugin`. No PATH search -- a `.lrplugin` is self-contained,
--- and Lightroom's sandbox could not read PATH to search it anyway.
local function find_binary(property_table)
	local plugin_dir = _PLUGIN and _PLUGIN.path or nil
	local bundled = plugin_dir and LrPathUtils.child(plugin_dir, 'tohdr') or nil

	return TohdrCli.locate_binary {
		user_binary_path = property_table.tohdr_binaryPath,
		plugin_binary_path = bundled,
		file_exists = function(p)
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
local log_counter = 0
local function temp_log_path()
	log_counter = log_counter + 1
	local entropy = tostring({}):gsub('%W', '')
	return LrPathUtils.child(
		LrPathUtils.getStandardFilePath('temp'),
		string.format('tohdr-%s-%d-%d.log',
			entropy, log_counter, math.floor(LrDate.currentTime()))
	)
end

--- The original camera file this rendition was developed from, or nil.
---
--- Used only for its `MakerNote`: Lightroom renders the raw's standard Exif into
--- the intermediate and drops the vendor block, so `tohdr` reads that one tag out
--- of the original. It reads about the first 43 KB of it, not the whole file.
---
--- Three things here are load-bearing, and all three are in the LrC 15.3 SDK
--- reference (API Reference/modules/SDK - LrPhoto.html):
---
---   * `getRawMetadata` "no longer needs to be called from within one of the
---     catalog:with___AccessDo gates, but must be called from within a task
---     started using LrTasks" -- and processRenderedPhotos already is one, so
---     this needs no gate of its own.
---   * `path` is "The current path to the photo file if available; otherwise, the
---     last known path to the file". *Last known* -- so it can name a file on an
---     unmounted volume, or a photo backed only by a smart preview. Existence has
---     to be checked, not assumed.
---   * A virtual copy has no file of its own; `masterPhoto` is the one that does.
---
--- Restricted to the formats that plausibly carry a MakerNote at all. `PSD` and
--- `TIFF` masters are scans and composites, and asking `tohdr` to look in one
--- would earn a "has no MakerNote to take" warning per photo -- a dialog full of
--- notices about files that were never going to have one.
local function takes_a_maker_note(format)
	return format == 'RAW' or format == 'DNG' or format == 'JPG'
end

--- The original camera file behind a rendition, or nil and the reason why not.
---
--- The reason is the whole point of the second return value. When the user has
--- asked for the camera's `MakerNote` and the catalog cannot produce a file to
--- take one from, the conversion still succeeds and `tohdr` is never told that
--- anything was wanted -- so it cannot warn, and the feature becomes a silent
--- no-op that looks exactly like success. That is precisely how the first live
--- export failed: engine `portable`, box checked, and nothing in the output to
--- say the original was never found.
---
--- Reasons are worded to be identical across photos, so 200 renditions with the
--- same cause collapse to one line with a count instead of 200 lines.
local function original_path(rendition)
	local photo = rendition.photo
	if not photo then
		return nil, 'Lightroom did not say which photo this rendition came from'
	end
	-- pcall because this is metadata access on a catalog we do not own: a photo
	-- can be removed mid-export, and losing one MakerNote must not fail a
	-- conversion that would otherwise succeed.
	local ok, path, format = pcall(function()
		local subject = photo
		if photo:getRawMetadata('isVirtualCopy') then
			subject = photo:getRawMetadata('masterPhoto') or photo
		end
		local fmt = subject:getRawMetadata('fileFormat')
		if not takes_a_maker_note(fmt) then
			return nil, fmt
		end
		return subject:getRawMetadata('path'), fmt
	end)
	if not ok then
		-- `path` holds the error when pcall failed. Kept verbatim rather than
		-- summarised: this is the one channel that can explain an API contract
		-- we got wrong, and paraphrasing it would throw that away.
		return nil, 'the catalog would not answer (' .. tostring(path) .. ')'
	end
	if not takes_a_maker_note(format) then
		return nil, 'the master is a ' .. tostring(format or 'file of unknown format')
			.. ', which carries no MakerNote'
	end
	if path == nil or path == '' then
		return nil, 'the catalog holds no path for the master file'
	end
	if LrFileUtils.exists(path) == false then
		return nil, 'the original file is no longer where the catalog expects it'
	end
	return path
end

local function run_capturing(command_line)
	local log_path = temp_log_path()
	local full = command_line .. ' >' .. TohdrCli.quote_arg(log_path) .. ' 2>&1'
	local status = LrTasks.execute(full)

	local output = ''
	if LrFileUtils.exists(log_path) then
		output = LrFileUtils.readFile(log_path) or ''
		LrFileUtils.delete(log_path)
	end
	return status, output
end

function export_service_provider.processRenderedPhotos(_function_context, export_context)
	local export_session = export_context.exportSession
	local property_table = export_context.propertyTable
	local n_photos = export_session:countRenditions()

	local progress = export_context:configureProgress {
		title = n_photos > 1
			and ('Converting ' .. n_photos .. ' photos to gain-map HEIC')
			or 'Converting one photo to gain-map HEIC',
	}

	local binary, locate_err = find_binary(property_table)

	-- Advisories from every photo, collected rather than reported one at a time:
	-- Lightroom has no per-photo channel for "succeeded, but you should know
	-- something", and 200 modal dialogs is not a design.
	local advisories = {}

	for _, rendition in export_context:renditions { stopIfCanceled = true } do
		-- Fail every rendition individually rather than aborting the export:
		-- Lightroom shows the message per photo, which is what the user needs
		-- to see when only some conversions fail.
		if not binary then
			rendition:uploadFailed(locate_err or 'tohdr binary not found')
		else
			local ok, path_or_message = rendition:waitForRender()
			if not ok then
				rendition:uploadFailed(path_or_message or 'Lightroom failed to render this photo')
			else
				local rendered_path = path_or_message
				local dest_dir = LrPathUtils.parent(rendition.destinationPath)
				local out_name = LrPathUtils.removeExtension(
					LrPathUtils.leafName(rendition.destinationPath)
				) .. '.heic'
				local out_path = LrPathUtils.child(dest_dir, out_name)

				-- Looked up only when it would be used, so a user who left the
				-- checkbox off pays no catalog access for it.
				local raw_path, no_raw_because
				if property_table.tohdr_makerNote then
					raw_path, no_raw_because = original_path(rendition)
				end

				local args = TohdrCli.build_convert_args(
					property_table, rendered_path, out_path, raw_path
				)
				local cmd = TohdrCli.build_command_line(binary, args)
				local status, output = run_capturing(cmd)

				if status ~= 0 then
					rendition:uploadFailed(TohdrCli.summarize_failure(status, output))
				elseif not LrFileUtils.exists(out_path) then
					rendition:uploadFailed(
						'tohdr reported success but wrote no file to ' .. out_path
					)
				elseif TohdrCli.gain_map_source(output) == 'derived' then
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
					LrFileUtils.delete(out_path)
					rendition:uploadFailed(
						'Lightroom rendered an SDR intermediate with no gain map, so the HEIC '
							.. 'would have carried no HDR. Check that this photo is in HDR edit '
							.. 'mode in Develop; if it is, this Lightroom version may not accept '
							.. 'the HDR export settings the plugin requests.'
					)
				else
					-- Succeeded. Keep anything `tohdr` said about how, so a
					-- silent fallback (most importantly a colour space it had to
					-- guess at) reaches the user instead of the bit bucket.
					TohdrCli.merge_advisories(advisories, TohdrCli.advisories(output))

					-- `tohdr` warns about every MakerNote it refuses, but it
					-- cannot warn about a file it was never handed. This is the
					-- one outcome only the plugin can see, so it is reported in
					-- the same channel and collapses by count like the rest.
					if no_raw_because then
						TohdrCli.merge_advisories(advisories, {
							{
								text = "warning: the camera's MakerNote was requested, but "
									.. no_raw_because,
								count = 1,
							},
						})
					end
				end

				-- Delete the intermediate on EVERY path, not just success. It
				-- is a full-resolution uncompressed 16-bit TIFF -- hundreds of
				-- MB for a real photo -- and the user never asked for it, so
				-- leaving one behind per failed rendition fills the render
				-- cache silently.
				if rendered_path ~= out_path then
					LrFileUtils.delete(rendered_path)
				end
			end
		end
	end

	if progress then
		progress:done()
	end

	-- Shown only when there is something to show. An export that got exactly
	-- what it asked for must stay silent: a dialog that always appears is one
	-- that always gets dismissed unread, which would defeat the point of
	-- collecting these at all.
	local summary = TohdrCli.summarize_advisories(advisories)
	if summary then
		LrDialogs.message('tohdr', summary, 'info')
	end
end

return export_service_provider
