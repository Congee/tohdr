--[[----------------------------------------------------------------------

TohdrCli.lua

Pure-Lua logic for building the `tohdr convert` command line and locating the
`tohdr` binary. Has ZERO `import 'LrXxx'` calls on purpose, so it runs under a
stock `lua` interpreter for tests/test_TohdrCli.lua; all filesystem access is
injected by the caller.

------------------------------------------------------------------------]]

local M = {}

-- ===========================================================================
-- Shell quoting
-- ===========================================================================

--- Quote one argument for POSIX `/bin/sh -c`, which is what LrTasks.execute
--- runs. Single quotes, because inside them nothing expands -- exactly what a
--- photo path full of spaces, unicode or literal `$` needs.
function M.quote_arg(s)
	assert(type(s) == "string", "quote_arg: expected a string")
	return "'" .. s:gsub("'", "'\\''") .. "'"
end

--- Build a command line from a binary path and plain unquoted args.
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

-- One copy of the accepted values, so the dialog's choices and the args we emit
-- cannot drift. Must match cli.rs parse_flavor / EngineKind::parse / parse_tone_map.
M.FLAVORS = { "apple", "iso", "both" }
M.ENGINES = { "apple", "portable" }
M.TONE_MAPS = { "clip", "reinhard" }

local function is_non_empty(v)
	return v ~= nil and v ~= ""
end

--- Build the argument array for `tohdr convert <input> --output <output> [...]`.
---
--- `raw_path` is an argument, not a setting: `tohdr_makerNote` is the user's
--- standing choice, this is the one per-photo file it applies to.
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

	-- Stated, not left to the CLI default, so a future default change cannot
	-- silently retag these exports. Redundant on the normal path (tohdr reads
	-- the intermediate's ICC profile); it decides the SDR-fallback case, where
	-- there is no profile to read.
	table.insert(args, "--colour-space")
	table.insert(args, "p3")

	if settings.tohdr_headroom then
		table.insert(args, "--headroom")
		table.insert(args, tostring(settings.tohdr_headroom))
	end

	-- The one thing the intermediate cannot carry: LrC renders the raw's Exif
	-- but drops the vendor MakerNote. Passed only when the user asked *and* a
	-- file was found, so absent means tohdr behaves as it did before.
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

-- There is deliberately no PATH search, and do not add one. The sandbox has no
-- `os.getenv`, so the code removed from here was not reading PATH but guessing
-- at it (homebrew, /usr/local, ~/.cargo, ~/.nix-profile) -- which could silently
-- run a stale `tohdr` from another prefix. A bundled binary is always installed,
-- so the guess was unreachable anyway. An explicit custom path still works,
-- being the user's choice rather than ours.

--- Turn what `LrTasks.execute` returns into the exit code a person expects.
---
--- macOS gives back the raw wait(2) status, which packs the exit code into the
--- high byte -- `tohdr` exiting 1 arrives as 256. Only exact multiples of 256
--- are unpacked: Windows returns a plain code, and POSIX signal deaths are
--- 128+signal, already the familiar rendering.
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

--- One message worth showing a user, from a nonzero status plus captured output.
--- The last non-empty line is `tohdr`'s own diagnosis and the actionable half.
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

--- Where a successful run's gain map came from: `"lightroom-embedded"`,
--- `"derived"`, or nil when the output says neither.
---
--- Reads the JSON, because with `--json` the CLI prints *only* that -- a gate
--- searching for the prose line could never match. The text forms are still
--- recognised for runs without `--json`, anchored to line start because the
--- *transcoded* message ends "...not derived".
---
--- nil means "do not fail the photo": a wrong accusation deletes a good file,
--- a missed one leaves a file the user can still look at.
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

--- Pull `note:`/`warning:` lines out of a successful run's output, deduplicated
--- with counts so a 200-photo export reports each condition once.
---
--- Matters most for the colour space: if LrC ignores the `p3_hdr` request,
--- `tohdr` says so here. Silence now means the request was honoured.
function M.advisories(output)
	local order, seen = {}, {}
	for line in tostring(output or ""):gmatch("[^\r\n]+") do
		-- Anchored on the CLI's own `tohdr:` prefix, so a filename containing
		-- the word cannot fake an advisory.
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

--- Render collected advisories into one message, or nil when there is nothing
--- to say -- a dialog that always appears gets clicked away unread.
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

--- Which `tohdr` to run: the user's custom path, else the bundled binary. No
--- third option, and nothing is located by guessing.
---
--- Existence goes through the injected `file_exists`, so this stays testable.
--- Returns `path, nil`, or `nil, error_message` for the caller to show.
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

	-- Name the expected location: it is the only automatic one, and the user has
	-- no other way to know where we looked.
	return nil, "Could not find the 'tohdr' binary. It belongs beside the "
		.. "plugin's .lua files"
		.. (is_non_empty(plugin_binary_path) and (" (expected at " .. plugin_binary_path .. ")") or "")
		.. " -- build it with `cargo build --release -p tohdr-cli` and copy it "
		.. "there, or set a custom path in the export dialog."
end

return M
