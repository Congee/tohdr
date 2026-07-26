--[[----------------------------------------------------------------------

TohdrExportDialog.lua

Builds the export-settings section shown in Lightroom's Export dialog:
gain-map flavor, engine, max output size, quality, and tone-map. This file
only constructs LrView widgets bound to `propertyTable` -- no CLI or process
logic lives here (see TohdrCli.lua for that).

------------------------------------------------------------------------]]

local LrView = import 'LrView'

local TohdrExportDialog = {}

local bind = LrView.bind
local share = LrView.share

--- Fill in defaults the first time the dialog opens for this preset/export.
--- Called from TohdrExportServiceProvider.startDialog.
function TohdrExportDialog.startDialog(propertyTable)
	local defaults = {
		tohdr_flavor = "both",
		tohdr_engine = "apple",
		tohdr_maxSizeEnabled = false,
		tohdr_maxSizeValue = 4,
		tohdr_maxSizeUnit = "MB",
		tohdr_quality = 85,
		tohdr_minQuality = 40,
		tohdr_toneMap = "reinhard",
		tohdr_binaryPath = "",
	}
	for k, v in pairs(defaults) do
		if propertyTable[k] == nil then
			propertyTable[k] = v
		end
	end
end

--- The dialog section. Returned as a one-element array per the
--- sectionsForTopOfDialog contract.
function TohdrExportDialog.sectionsForTopOfDialog(f, propertyTable)
	local labelWidth = share 'tohdr_labelWidth'

	return {
		{
			title = "HDR Gain-Map HEIC (tohdr)",

			f:row {
				f:static_text {
					title = "Flavor:",
					alignment = "right",
					width = labelWidth,
				},
				f:popup_menu {
					value = bind 'tohdr_flavor',
					items = {
						{ title = "Apple (HDRGainMap)", value = "apple" },
						{ title = "ISO 21496-1", value = "iso" },
						{ title = "Both", value = "both" },
					},
				},
			},

			f:row {
				f:static_text {
					title = "Engine:",
					alignment = "right",
					width = labelWidth,
				},
				-- Two engines, three values. Engine A is ImageIO. Engine B is our
				-- muxer over a plane codec, and it has two codecs:
				-- `portable` picks the fastest this machine has -- VideoToolbox,
				-- the hardware media block, ~6x the software codec -- while
				-- `hpvca` pins the pure-Rust one, which is the reference path.
				--
				-- "Portable (pure Rust)" used to be the label on `portable`,
				-- which actually runs Apple's hardware encoder: it reports
				-- itself as `hardware-videotoolbox`. That named the one option
				-- the menu did not offer.
				f:popup_menu {
					value = bind 'tohdr_engine',
					items = {
						{ title = "Apple (ImageIO)", value = "apple" },
						{ title = "Portable (hardware, fastest)", value = "portable" },
						{ title = "Portable (pure Rust)", value = "hpvca" },
					},
				},
			},

			f:separator { fill_horizontal = 1 },

			f:row {
				f:checkbox {
					title = "Limit output file size to:",
					value = bind 'tohdr_maxSizeEnabled',
				},
				f:edit_field {
					value = bind 'tohdr_maxSizeValue',
					precision = 2,
					width_in_digits = 6,
					min = 0.1,
					max = 1000,
					enabled = bind 'tohdr_maxSizeEnabled',
				},
				f:popup_menu {
					value = bind 'tohdr_maxSizeUnit',
					enabled = bind 'tohdr_maxSizeEnabled',
					items = {
						{ title = "MB", value = "MB" },
						{ title = "MiB", value = "MiB" },
					},
				},
			},
			f:row {
				f:spacer { width = labelWidth },
				f:static_text {
					title = "tohdr re-encodes down to --min-quality below if the first pass overshoots.",
					enabled = bind 'tohdr_maxSizeEnabled',
				},
			},

			f:separator { fill_horizontal = 1 },

			f:row {
				f:static_text {
					title = "Quality:",
					alignment = "right",
					width = labelWidth,
				},
				f:edit_field {
					value = bind 'tohdr_quality',
					precision = 0,
					width_in_digits = 3,
					min = 1,
					max = 100,
				},
				f:static_text { title = "Min quality (size search floor):" },
				f:edit_field {
					value = bind 'tohdr_minQuality',
					precision = 0,
					width_in_digits = 3,
					min = 1,
					max = 100,
				},
			},

			f:row {
				f:static_text {
					title = "Tone map:",
					alignment = "right",
					width = labelWidth,
				},
				f:popup_menu {
					value = bind 'tohdr_toneMap',
					items = {
						{ title = "Reinhard", value = "reinhard" },
						{ title = "Clip", value = "clip" },
					},
				},
			},

			f:separator { fill_horizontal = 1 },

			f:row {
				f:static_text {
					title = "tohdr path:",
					alignment = "right",
					width = labelWidth,
				},
				f:edit_field {
					value = bind 'tohdr_binaryPath',
					fill_horizontal = 1,
				},
				f:push_button {
					title = "Choose...",
					action = function()
						local LrDialogs = import 'LrDialogs'
						local path = LrDialogs.runOpenPanel {
							title = "Locate the tohdr binary",
							canChooseFiles = true,
							canChooseDirectories = false,
							allowsMultipleSelection = false,
						}
						if path and path[1] then
							propertyTable.tohdr_binaryPath = path[1]
						end
					end,
				},
			},
			f:row {
				f:spacer { width = labelWidth },
				f:static_text {
					title = "Leave blank to auto-detect (bundled binary, then PATH).",
				},
			},
		},
	}
end

return TohdrExportDialog
