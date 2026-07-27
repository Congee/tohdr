--[[----------------------------------------------------------------------

TohdrExportDialog.lua

Builds the export-settings section shown in Lightroom's Export dialog:
gain-map flavor, engine, max output size, quality, and tone-map. This file
only constructs LrView widgets bound to `property_table` -- no CLI or process
logic lives here (see TohdrCli.lua for that).

------------------------------------------------------------------------]]

local LrView = import 'LrView'

local M = {}

local bind = LrView.bind
local share = LrView.share

--- Fill in defaults the first time the dialog opens for this preset/export.
--- Assigned to Adobe's `startDialog` field by ExportServiceProvider.lua.
function M.start_dialog(property_table)
	local defaults = {
		tohdr_flavor = "both",
		tohdr_engine = "apple",
		tohdr_maxSizeEnabled = false,
		tohdr_maxSizeValue = 4,
		tohdr_maxSizeUnit = "MB",
		tohdr_quality = 85,
		tohdr_minQuality = 40,
		tohdr_toneMap = "reinhard",
		tohdr_makerNote = true,
		tohdr_binaryPath = "",
	}
	for k, v in pairs(defaults) do
		if property_table[k] == nil then
			property_table[k] = v
		end
	end
end

--- The dialog section. Returned as a one-element array per the
--- sectionsForTopOfDialog contract.
function M.sections_for_top_of_dialog(f, property_table)
	local label_width = share 'tohdr_labelWidth'

	return {
		{
			title = "HDR Gain-Map HEIC (tohdr)",

			f:row {
				f:static_text {
					title = "Flavor:",
					alignment = "right",
					width = label_width,
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
					width = label_width,
				},
				-- Two engines, three values: `videotoolbox` is Apple's media
				-- block (~6x software), `hpvca` pins the pure-Rust reference
				-- path. Only `hpvca` is portable; the other two need macOS.
				f:popup_menu {
					value = bind 'tohdr_engine',
					items = {
						{ title = "Apple (ImageIO)", value = "apple" },
						{ title = "VideoToolbox (hardware, fastest)", value = "videotoolbox" },
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
				f:spacer { width = label_width },
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
					width = label_width,
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
					width = label_width,
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
				f:spacer { width = label_width },
				f:checkbox {
					title = "Copy the camera's MakerNote from the original file",
					value = bind 'tohdr_makerNote',
				},
			},
			f:row {
				f:spacer { width = label_width },
				-- The trap worth spelling out: ImageIO's property model has a key
				-- for Apple's MakerNote and none for anyone else's, so a Sony
				-- block survives only on the two non-Apple engines.
				f:static_text {
					title = "Lens, shutter count, creative style. Needs a non-Apple engine --\n"
						.. "the Apple engine writes only Apple's. Describes the capture,\n"
						.. "not your edit, so as-shot white balance and style stay as shot.",
					height_in_lines = 3,
					enabled = bind 'tohdr_makerNote',
				},
			},

			f:separator { fill_horizontal = 1 },

			f:row {
				f:static_text {
					title = "tohdr path:",
					alignment = "right",
					width = label_width,
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
							property_table.tohdr_binaryPath = path[1]
						end
					end,
				},
			},
			f:row {
				f:spacer { width = label_width },
				f:static_text {
					-- Not "then PATH": there is no PATH search and cannot be one --
					-- Lightroom's sandbox has no os.getenv. See TohdrCli.lua.
					title = "Leave blank to use the binary bundled beside the plugin.",
				},
			},
		},
	}
end

return M
