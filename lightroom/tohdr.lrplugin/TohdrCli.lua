--[[----------------------------------------------------------------------

TohdrCli.lua

Pure-Lua logic for building the `tohdr convert` command line and locating
the `tohdr` binary. Deliberately has ZERO `import 'LrXxx'` calls so it can
be loaded and unit-tested with a stock `lua`/`luajit` interpreter outside
Lightroom -- see tests/test_TohdrCli.lua. All filesystem/PATH/plugin-path
access is pushed to the caller as plain values or injected functions.

------------------------------------------------------------------------]]

local M = {}

-- ===========================================================================
-- Shell quoting
-- ===========================================================================

--- Quote a single argument for POSIX `/bin/sh -c "..."` (what
--- LrTasks.execute runs on macOS). Wrap in single quotes and escape any
--- embedded single quote as '\'' (close quote, escaped quote, reopen quote).
--- Single-quoting is chosen over double-quoting because inside single quotes
--- *nothing* is special -- no `$`, `` ` ``, `\`, or `"` expansion -- which is
--- exactly what a photo path (spaces, unicode, even literal `$` or `` ` ``)
--- needs.
function M.quote_arg(s)
	assert(type(s) == "string", "quote_arg: expected a string")
	return "'" .. s:gsub("'", "'\\''") .. "'"
end

--- Build a full shell command line from a binary path and an array of plain
--- (unquoted) argument strings. Every element is quoted independently.
function M.build_command_line(binary_path, args)
	local parts = { M.quote_arg(binary_path) }
	for _, a in ipairs(args) do
		table.insert(parts, M.quote_arg(tostring(a)))
	end
	return table.concat(parts, " ")
end

-- ===========================================================================
-- `tohdr convert` argument construction
-- ===========================================================================

--- Recognized flavor/engine/tone-map values, kept in one place so the
--- dialog's combo-box choices and the args we emit can't drift apart. Must
--- match crates/tohdr-cli/src/cli.rs's parse_flavor / EngineKind::parse /
--- parse_tone_map.
M.FLAVORS = { "apple", "iso", "both" }
M.ENGINES = { "apple", "portable" }
M.TONE_MAPS = { "clip", "reinhard" }

local function is_non_empty(v)
	return v ~= nil and v ~= ""
end

--- Build the argument array (unquoted plain strings) for
--- `tohdr convert <input> --output <output> [...]` from export settings.
---
--- `settings` fields consumed (all optional except flavor/engine/quality/
--- min_quality/tone_map, which the dialog always sets a default for):
---   tohdr_flavor            "apple" | "iso" | "both"
---   tohdr_engine            "apple" | "portable"
---   tohdr_maxSizeEnabled    boolean
---   tohdr_maxSizeValue      number, e.g. 4
---   tohdr_maxSizeUnit       "MB" | "MiB"
---   tohdr_quality           number 1..100
---   tohdr_minQuality        number 1..100
---   tohdr_toneMap           "clip" | "reinhard"
---   tohdr_gainSubsample     number (optional; omitted -> CLI default of 2)
---   tohdr_headroom          number (optional; omitted -> CLI auto-derives it)
---   tohdr_makerNote         boolean; pass `raw_path` to `--maker-note-from`
---
--- `raw_path` is the original camera file this rendition was developed from, when
--- the caller could find one. Optional and per-photo, so it is an argument rather
--- than a setting: `tohdr_makerNote` is the user's standing choice, this is the
--- one file that choice applies to.
function M.build_convert_args(settings, input_path, output_path, raw_path)
	assert(is_non_empty(input_path), "build_convert_args: input_path is required")
	assert(is_non_empty(output_path), "build_convert_args: output_path is required")

	local args = { "convert", input_path, "--output", output_path }

	if is_non_empty(settings.tohdr_flavor) then
		table.insert(args, "--flavor")
		table.insert(args, settings.tohdr_flavor)
	end

	if is_non_empty(settings.tohdr_engine) then
		table.insert(args, "--engine")
		table.insert(args, settings.tohdr_engine)
	end

	if settings.tohdr_maxSizeEnabled and settings.tohdr_maxSizeValue then
		local unit = is_non_empty(settings.tohdr_maxSizeUnit) and settings.tohdr_maxSizeUnit or "MB"
		table.insert(args, "--max-size")
		table.insert(args, tostring(settings.tohdr_maxSizeValue) .. unit)
	end

	if settings.tohdr_quality then
		table.insert(args, "--quality")
		table.insert(args, tostring(settings.tohdr_quality))
	end

	if settings.tohdr_minQuality then
		table.insert(args, "--min-quality")
		table.insert(args, tostring(settings.tohdr_minQuality))
	end

	if is_non_empty(settings.tohdr_toneMap) then
		table.insert(args, "--tone-map")
		table.insert(args, settings.tohdr_toneMap)
	end

	if settings.tohdr_gainSubsample then
		table.insert(args, "--gain-subsample")
		table.insert(args, tostring(settings.tohdr_gainSubsample))
	end

	-- Stated rather than left to the CLI's default, so the plugin's two halves
	-- cannot drift: ExportServiceProvider asks Lightroom for a `p3_hdr`
	-- intermediate, and this is the same decision spelled to the other side. A
	-- future change to the CLI's default must not silently retag these exports.
	--
	-- On the normal path `tohdr` reads the intermediate's own ICC profile and this
	-- flag is redundant; it decides the SDR-fallback case, where there is no
	-- embedded gain map and nothing to read a profile from.
	table.insert(args, "--colour-space")
	table.insert(args, "p3")

	if settings.tohdr_headroom then
		table.insert(args, "--headroom")
		table.insert(args, tostring(settings.tohdr_headroom))
	end

	-- The one thing the intermediate cannot carry. Lightroom renders most of what
	-- the raw says about the photograph into the TIFF's Exif and none of the
	-- vendor MakerNote, so `tohdr` reads that block out of the original instead.
	-- Both conditions matter: the user asked for it, and we actually found a file
	-- to read. Nothing is passed when either is missing, and then `tohdr` behaves
	-- exactly as it did before this existed.
	if settings.tohdr_makerNote and is_non_empty(raw_path) then
		table.insert(args, "--maker-note-from")
		table.insert(args, raw_path)
	end

	table.insert(args, "--json")

	return args
end

-- ===========================================================================
-- Binary location
-- ===========================================================================

-- There is deliberately no PATH search here, and no `defaultPathEnv` /
-- `splitPath` to support one. `.lrplugin` bundles are self-contained: the
-- binary is installed beside these `.lua` files, and that is the only
-- automatic lookup.
--
-- What was removed, and why it could not be repaired:
--
--   * There is no PATH to read. Lightroom's Lua sandbox has no `os.getenv` and
--     no Lr API exposes the environment, so the old code was not searching
--     `PATH` -- it was searching a hardcoded guess at it (/opt/homebrew/bin,
--     /usr/local/bin, ~/.cargo/bin, ~/.nix-profile/bin,
--     /run/current-system/sw/bin). That guess encodes package-manager
--     assumptions with no way to check them.
--   * It was unreachable anyway. A bundled binary is checked first and the
--     install step always provides one, so the guess only ever ran in a
--     configuration that was already broken.
--   * It was actively dangerous. A stale `tohdr` in one of those prefixes
--     would be found and used silently, converting with a build other than the
--     one you just made -- exactly the class of quiet wrongness this project
--     exists to prevent.
--
-- An explicit **Custom tohdr path** still works, because that is a deliberate
-- choice by the user rather than a guess by us; it is what to point at
-- `target/release/tohdr` during development.

--- Turn what `LrTasks.execute` returns into the exit code a person expects.
---
--- The SDK documents the return as "the exit status of the OS shell", and the
--- call as "similar to Lua's built-in os.execute()". On macOS that is the raw
--- wait(2) status from system(3), which packs a normal exit code into the high
--- byte. So `tohdr` exiting 1 arrives here as 256 -- and the first real export
--- inside Lightroom duly reported "tohdr failed (exit 256)" to the user, a
--- number that appears nowhere in the CLI.
---
--- Only exact multiples of 256 are unpacked. Everything else passes through:
--- Windows returns a plain exit code, and on POSIX a signal death shows up as
--- 128+signal, which is already the familiar shell rendering. A small nonzero
--- value is genuinely ambiguous between those two platforms and there is no
--- information here to resolve it, so this does not pretend to.
function M.decode_exit_status(status)
	local n = tonumber(status)
	if n == nil then
		return status
	end
	if n >= 256 and n % 256 == 0 then
		return math.floor(n / 256)
	end
	return n
end

--- Turn a nonzero exit status plus captured output into one message worth
--- showing a user.
---
--- `tohdr` prints its own diagnosis on stderr (which quality it tried, why a
--- budget could not be met, and what to change), so the last non-empty line is
--- almost always the actionable part. Swallowing it and reporting only an exit
--- code would throw away the useful half.
function M.summarize_failure(status, output)
	status = M.decode_exit_status(status)
	local last
	for line in tostring(output or ""):gmatch("[^\r\n]+") do
		if line:match("%S") then
			last = line
		end
	end
	if last then
		return "tohdr failed (exit " .. tostring(status) .. "): " .. last
	end
	return "tohdr failed (exit " .. tostring(status) .. ") with no output"
end

--- Where the gain map in a successful run's output came from.
---
--- `"lightroom-embedded"` when `tohdr` transcoded the intermediate's own gain
--- map, `"derived"` when it computed one from the pixels, `nil` when the output
--- says neither.
---
--- This reads the JSON, not the prose, and that is the whole point of it. The
--- plugin passes `--json`, and with `--json` the CLI prints *only* the JSON object
--- -- the human-readable "  gain map: derived from the source's HDR pixels" line
--- lives in the other branch of `convert::run` and is never emitted. So the gate
--- that searched for that line could not match, and an SDR intermediate would have
--- produced a washed-out HEIC reported as a success: the one failure this plugin
--- exists to catch, silently unguarded.
---
--- Matched as a pattern rather than parsed, because Lightroom's Lua has no JSON
--- decoder and this is one flat object of scalars from a serde struct -- no
--- nesting, no escapes in this field's value. The text forms are still recognized
--- so a run without `--json` is not a blind spot, and they are anchored to the
--- start of a line for two reasons: a filename containing "gain map: derived"
--- would otherwise fake one, and the *transcoded* line ends "...not derived", so a
--- plain substring search for the word finds it in the message that means the
--- opposite.
---
--- `nil` for an output that names no source -- an older binary, or a future one
--- that renames the field. Callers must treat that as "do not fail the photo": a
--- wrong accusation deletes a good file, where a missed one leaves a file the user
--- can still look at.
function M.gain_map_source(output)
	local text = tostring(output or "")
	local from_json = text:match('"gain_map_source"%s*:%s*"([^"]*)"')
	if from_json then
		return from_json
	end
	for line in text:gmatch("[^\r\n]+") do
		local word = line:match("^%s*gain map:%s*(%a+)")
		if word == "transcoded" then
			return "lightroom-embedded"
		elseif word == "derived" then
			return "derived"
		end
	end
	return nil
end

--- Pull the advisory lines out of a *successful* run's output.
---
--- `tohdr` prints `note:` when it did something the user did not ask for but
--- which is defensible, and `warning:` when it had to guess or drop something.
--- The colour-space report is the case that matters most here: the plugin asks
--- Lightroom for `p3_hdr` and asks `tohdr` for `--colour-space p3`, and if
--- Lightroom ignores that request `tohdr` says so -- either
---
---   note: <file> is srgb by its own ICC profile, so the output declares ...
---   warning: <file> embeds no ICC profile this build recognises ...
---
--- Until this function existed, both lines were captured and then thrown away on
--- the success path, so a Lightroom that quietly exported the wrong colour space
--- produced a HEIC labelled from a default instead of from the file, and nothing
--- told anybody. Silence from an export now means the request was honoured.
---
--- Deduplicated with counts, because a 200-photo export hits the same condition
--- 200 times and one dialog listing it once is the readable form. Returns a list
--- of `{ text = <line>, count = <n> }` in first-seen order, empty when the run
--- had nothing to say.
function M.advisories(output)
	local order, seen = {}, {}
	for line in tostring(output or ""):gmatch("[^\r\n]+") do
		-- Match the CLI's own prefix rather than a bare "note:", so a filename
		-- or an Exif string containing the word cannot fake an advisory.
		local text = line:match("^%s*tohdr:%s*(note:.*)$") or line:match("^%s*tohdr:%s*(warning:.*)$")
		if text then
			if seen[text] then
				seen[text].count = seen[text].count + 1
			else
				seen[text] = { text = text, count = 1 }
				order[#order + 1] = seen[text]
			end
		end
	end
	return order
end

--- Render what `advisories` collected across a whole export into one message.
---
--- Returns nil when there is nothing to report, so the caller can skip showing a
--- dialog at all -- an export that went exactly as asked must stay silent, or the
--- dialog becomes noise that gets clicked away without reading.
function M.summarize_advisories(list)
	if not list or #list == 0 then
		return nil
	end
	local lines = {}
	for _, item in ipairs(list) do
		lines[#lines + 1] = (item.count > 1)
			and ("- " .. item.text .. " (" .. tostring(item.count) .. " photos)")
			or ("- " .. item.text)
	end
	return "The conversion succeeded, with notes:\n\n" .. table.concat(lines, "\n")
end

--- Merge one run's advisories into a running list, preserving order and counts.
function M.merge_advisories(into, list)
	for _, item in ipairs(list or {}) do
		local found
		for _, existing in ipairs(into) do
			if existing.text == item.text then
				found = existing
				break
			end
		end
		if found then
			found.count = found.count + item.count
		else
			into[#into + 1] = { text = item.text, count = item.count }
		end
	end
	return into
end

--- Decide which `tohdr` binary to run, in priority order:
---   1. explicit user-configured path (settings dialog "Custom tohdr path")
---   2. the bundled binary beside the plugin (plugin_binary_path)
---
--- There is no third option -- see the note above `summarize_failure`. Nothing
--- is ever located by guessing.
---
--- All existence checks go through the injected `file_exists(path) -> bool`
--- so this function has no direct filesystem access and is fully testable
--- with a fake.
---
--- Returns `path, nil` on success, or `nil, error_message` if nothing was
--- found -- callers should show `error_message` to the user rather than
--- fail silently.
function M.locate_binary(opts)
	local user_path = opts.user_binary_path
	local plugin_binary_path = opts.plugin_binary_path
	local file_exists = assert(opts.file_exists, "locate_binary: file_exists is required")

	if is_non_empty(user_path) then
		if file_exists(user_path) then
			return user_path, nil
		end
		return nil, "The configured tohdr path does not exist: " .. user_path
	end

	if is_non_empty(plugin_binary_path) and file_exists(plugin_binary_path) then
		return plugin_binary_path, nil
	end

	-- Name the expected location, because it is now the only automatic one and
	-- a user has no other way to guess where we looked.
	return nil, "Could not find the 'tohdr' binary. It belongs beside the "
		.. "plugin's .lua files"
		.. (is_non_empty(plugin_binary_path) and (" (expected at " .. plugin_binary_path .. ")") or "")
		.. " -- build it with `cargo build --release -p tohdr-cli` and copy it "
		.. "there, or set a custom path in the export dialog."
end

return M
