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
function M.quoteArg(s)
	assert(type(s) == "string", "quoteArg: expected a string")
	return "'" .. s:gsub("'", "'\\''") .. "'"
end

--- Build a full shell command line from a binary path and an array of plain
--- (unquoted) argument strings. Every element is quoted independently.
function M.buildCommandLine(binaryPath, args)
	local parts = { M.quoteArg(binaryPath) }
	for _, a in ipairs(args) do
		table.insert(parts, M.quoteArg(tostring(a)))
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

local function isNonEmpty(v)
	return v ~= nil and v ~= ""
end

--- Build the argument array (unquoted plain strings) for
--- `tohdr convert <input> --output <output> [...]` from export settings.
---
--- `settings` fields consumed (all optional except flavor/engine/quality/
--- minQuality/toneMap, which the dialog always sets a default for):
---   tohdr_flavor        "apple" | "iso" | "both"
---   tohdr_engine        "apple" | "portable"
---   tohdr_maxSizeEnabled boolean
---   tohdr_maxSizeValue  number, e.g. 4
---   tohdr_maxSizeUnit   "MB" | "MiB"
---   tohdr_quality       number 1..100
---   tohdr_minQuality    number 1..100
---   tohdr_toneMap       "clip" | "reinhard"
---   tohdr_gainSubsample number (optional; omitted -> CLI default of 2)
---   tohdr_headroom      number (optional; omitted -> CLI auto-derives it)
function M.buildConvertArgs(settings, inputPath, outputPath)
	assert(isNonEmpty(inputPath), "buildConvertArgs: inputPath is required")
	assert(isNonEmpty(outputPath), "buildConvertArgs: outputPath is required")

	local args = { "convert", inputPath, "--output", outputPath }

	if isNonEmpty(settings.tohdr_flavor) then
		table.insert(args, "--flavor")
		table.insert(args, settings.tohdr_flavor)
	end

	if isNonEmpty(settings.tohdr_engine) then
		table.insert(args, "--engine")
		table.insert(args, settings.tohdr_engine)
	end

	if settings.tohdr_maxSizeEnabled and settings.tohdr_maxSizeValue then
		local unit = isNonEmpty(settings.tohdr_maxSizeUnit) and settings.tohdr_maxSizeUnit or "MB"
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

	if isNonEmpty(settings.tohdr_toneMap) then
		table.insert(args, "--tone-map")
		table.insert(args, settings.tohdr_toneMap)
	end

	if settings.tohdr_gainSubsample then
		table.insert(args, "--gain-subsample")
		table.insert(args, tostring(settings.tohdr_gainSubsample))
	end

	if settings.tohdr_headroom then
		table.insert(args, "--headroom")
		table.insert(args, tostring(settings.tohdr_headroom))
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
function M.decodeExitStatus(status)
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
function M.summarizeFailure(status, output)
	status = M.decodeExitStatus(status)
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

--- Decide which `tohdr` binary to run, in priority order:
---   1. explicit user-configured path (settings dialog "Custom tohdr path")
---   2. the bundled binary beside the plugin (pluginBinaryPath)
---
--- There is no third option -- see the note above `summarizeFailure`. Nothing
--- is ever located by guessing.
---
--- All existence checks go through the injected `fileExists(path) -> bool`
--- so this function has no direct filesystem access and is fully testable
--- with a fake.
---
--- Returns `path, nil` on success, or `nil, errorMessage` if nothing was
--- found -- callers should show `errorMessage` to the user rather than
--- fail silently.
function M.locateBinary(opts)
	local userPath = opts.userBinaryPath
	local pluginBinaryPath = opts.pluginBinaryPath
	local fileExists = assert(opts.fileExists, "locateBinary: fileExists is required")

	if isNonEmpty(userPath) then
		if fileExists(userPath) then
			return userPath, nil
		end
		return nil, "The configured tohdr path does not exist: " .. userPath
	end

	if isNonEmpty(pluginBinaryPath) and fileExists(pluginBinaryPath) then
		return pluginBinaryPath, nil
	end

	-- Name the expected location, because it is now the only automatic one and
	-- a user has no other way to guess where we looked.
	return nil, "Could not find the 'tohdr' binary. It belongs beside the "
		.. "plugin's .lua files"
		.. (isNonEmpty(pluginBinaryPath) and (" (expected at " .. pluginBinaryPath .. ")") or "")
		.. " -- build it with `cargo build --release -p tohdr-cli` and copy it "
		.. "there, or set a custom path in the export dialog."
end

return M
