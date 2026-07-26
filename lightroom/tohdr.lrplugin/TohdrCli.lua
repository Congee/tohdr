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

--- Read an environment variable without assuming `os.getenv` exists.
---
--- Lightroom Classic's Lua sandbox does not provide `os.getenv` at all. Calling
--- it raises `attempt to call field 'getenv' (a nil value)`, which is how this
--- was found: an "Unable to Export" dialog on the first real export inside
--- Lightroom. Stock Lua (the test harness) does have it, so probe rather than
--- pick one behaviour.
local function env(name)
	local getenv = type(os) == "table" and os.getenv or nil
	if type(getenv) ~= "function" then
		return nil
	end
	local ok, value = pcall(getenv, name)
	if ok then
		return value
	end
	return nil
end

--- The PATH to search when neither an explicit path nor a bundled binary is
--- available.
---
--- Lightroom Classic launches from Finder, so even a process that *could* read
--- its environment would see a minimal PATH -- typically
--- `/usr/bin:/bin:/usr/sbin:/sbin`, without Homebrew, Cargo or Nix, which is
--- where a `tohdr` build actually lives. We therefore append the usual install
--- prefixes rather than trusting the inherited value alone.
---
--- Inside Lightroom the inherited PATH is not merely minimal, it is
--- unobservable: there is no `os.getenv` and no Lr API that exposes the
--- environment. The caller passes what it *can* discover:
---
---   opts.inheritedPath  a PATH-style string, or nil when unavailable
---   opts.home           the user's home directory, or nil
---
--- Both are optional; outside Lightroom they fall back to `env()` above, so a
--- zero-argument call still behaves as it always did under plain `lua`.
function M.defaultPathEnv(opts)
	opts = opts or {}
	local inherited = opts.inheritedPath or env("PATH") or ""
	local home = opts.home or env("HOME")

	-- Order is lookup precedence, so keep it stable.
	local extra = {
		"/opt/homebrew/bin",       -- Homebrew, Apple silicon
		"/usr/local/bin",          -- Homebrew, Intel; most `make install`s
	}
	-- Only when a home directory is actually known -- a bare "/.cargo/bin" is
	-- a real directory someone could create, so never synthesise one.
	if isNonEmpty(home) then
		table.insert(extra, home .. "/.cargo/bin")
		table.insert(extra, home .. "/.nix-profile/bin")
	end
	table.insert(extra, "/run/current-system/sw/bin")

	local parts = { inherited }
	for _, d in ipairs(extra) do
		table.insert(parts, d)
	end
	return table.concat(parts, ":")
end

--- Turn a nonzero exit status plus captured output into one message worth
--- showing a user.
---
--- `tohdr` prints its own diagnosis on stderr (which quality it tried, why a
--- budget could not be met, and what to change), so the last non-empty line is
--- almost always the actionable part. Swallowing it and reporting only an exit
--- code would throw away the useful half.
function M.summarizeFailure(status, output)
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

--- Split a PATH-style string (colon-separated) into a list of directories.
--- Empty entries (leading/trailing/doubled colons, POSIX "current dir") are
--- dropped -- we never want to silently execute a `tohdr` from cwd.
function M.splitPath(pathEnv)
	local dirs = {}
	if not isNonEmpty(pathEnv) then
		return dirs
	end
	for dir in pathEnv:gmatch("[^:]+") do
		table.insert(dirs, dir)
	end
	return dirs
end

--- Decide which `tohdr` binary to run, in priority order:
---   1. explicit user-configured path (settings dialog "Custom path")
---   2. bundled binary next to the plugin (pluginBinaryPath)
---   3. first match walking `pathDirs` (already-split PATH, each joined with
---      "/tohdr" by the caller-supplied `joinPath`)
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
	local pathDirs = opts.pathDirs or {}
	local fileExists = assert(opts.fileExists, "locateBinary: fileExists is required")
	local joinPath = opts.joinPath or function(dir, name)
		return dir .. "/" .. name
	end
	local binaryName = opts.binaryName or "tohdr"

	if isNonEmpty(userPath) then
		if fileExists(userPath) then
			return userPath, nil
		end
		return nil, "The configured tohdr path does not exist: " .. userPath
	end

	if isNonEmpty(pluginBinaryPath) and fileExists(pluginBinaryPath) then
		return pluginBinaryPath, nil
	end

	for _, dir in ipairs(pathDirs) do
		local candidate = joinPath(dir, binaryName)
		if fileExists(candidate) then
			return candidate, nil
		end
	end

	return nil, "Could not find the 'tohdr' binary. Bundle it next to the plugin, "
		.. "put it on your PATH, or set a custom path in the export dialog."
end

return M
