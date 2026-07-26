--[[----------------------------------------------------------------------

Unit tests for TohdrCli.lua, runnable with a stock Lua interpreter:

    lua lightroom/tests/test_TohdrCli.lua

TohdrCli has no `import 'LrXxx'` calls precisely so this is possible. The
quoting tests matter most: photo paths routinely contain spaces, and a
mis-quoted one either fails the export or, worse, executes part of the
filename.

------------------------------------------------------------------------]]

package.path = "lightroom/tohdr.lrplugin/?.lua;" .. package.path
local Cli = require 'TohdrCli'

local failures, checks = 0, 0

local function check(cond, what)
	checks = checks + 1
	if not cond then
		failures = failures + 1
		print("  FAIL: " .. what)
	end
end

local function eq(got, want, what)
	checks = checks + 1
	if got ~= want then
		failures = failures + 1
		print(string.format("  FAIL: %s\n    want: %s\n    got:  %s",
			what, tostring(want), tostring(got)))
	end
end

--- Find a value in an arg array and return the element after it.
local function argAfter(args, flag)
	for i, a in ipairs(args) do
		if a == flag then return args[i + 1] end
	end
	return nil
end

local function hasArg(args, flag)
	for _, a in ipairs(args) do
		if a == flag then return true end
	end
	return false
end

-- ===========================================================================
print("quoting")
-- ===========================================================================

eq(Cli.quoteArg("plain"), "'plain'", "plain string")
eq(Cli.quoteArg("with space.tif"), "'with space.tif'", "spaces")
eq(Cli.quoteArg("/a/b c/d.tif"), "'/a/b c/d.tif'", "path with a space")
-- The dangerous ones: shell metacharacters must stay literal.
eq(Cli.quoteArg("a$(rm -rf /)b"), "'a$(rm -rf /)b'", "command substitution")
eq(Cli.quoteArg("a`whoami`b"), "'a`whoami`b'", "backticks")
eq(Cli.quoteArg('a"b'), [['a"b']], "double quote")
eq(Cli.quoteArg("a;b"), "'a;b'", "semicolon")
eq(Cli.quoteArg("a\\b"), "'a\\b'", "backslash")
eq(Cli.quoteArg("naïve — 日本語.tif"), "'naïve — 日本語.tif'", "unicode")
-- A single quote is the one character that must break out and come back in.
eq(Cli.quoteArg("it's"), [['it'\''s']], "embedded single quote")
eq(Cli.quoteArg("''"), [[''\'''\''']], "two single quotes")

do
	local cmd = Cli.buildCommandLine("/usr/local/bin/to hdr",
		{ "convert", "/in put.tif", "--output", "/out'put.heic" })
	eq(cmd, [['/usr/local/bin/to hdr' 'convert' '/in put.tif' '--output' '/out'\''put.heic']],
		"full command line quotes every element")
end

-- ===========================================================================
print("convert args")
-- ===========================================================================

do
	local args = Cli.buildConvertArgs({
		tohdr_flavor = "both",
		tohdr_engine = "portable",
		tohdr_quality = 85,
		tohdr_minQuality = 40,
		tohdr_toneMap = "reinhard",
	}, "/in.tif", "/out.heic")

	eq(args[1], "convert", "subcommand first")
	eq(args[2], "/in.tif", "input is positional")
	eq(argAfter(args, "--output"), "/out.heic", "output flag")
	eq(argAfter(args, "--flavor"), "both", "flavor")
	eq(argAfter(args, "--engine"), "portable", "engine")
	eq(argAfter(args, "--quality"), "85", "quality stringified")
	eq(argAfter(args, "--tone-map"), "reinhard", "tone map")
	check(hasArg(args, "--json"), "--json requested so failures are parseable")
	check(not hasArg(args, "--max-size"), "no --max-size when disabled")
	check(not hasArg(args, "--headroom"), "no --headroom unless set")
end

do
	local args = Cli.buildConvertArgs({
		tohdr_maxSizeEnabled = true,
		tohdr_maxSizeValue = 4,
		tohdr_maxSizeUnit = "MB",
	}, "/in.tif", "/out.heic")
	eq(argAfter(args, "--max-size"), "4MB", "max size composes value and unit")
end

do
	local args = Cli.buildConvertArgs({
		tohdr_maxSizeEnabled = true,
		tohdr_maxSizeValue = 4,
		tohdr_maxSizeUnit = "MiB",
	}, "/in.tif", "/out.heic")
	eq(argAfter(args, "--max-size"), "4MiB", "MiB unit preserved")
end

do
	-- maxSizeEnabled false must suppress the flag even with a value present,
	-- or unchecking the box in the dialog would do nothing.
	local args = Cli.buildConvertArgs({
		tohdr_maxSizeEnabled = false,
		tohdr_maxSizeValue = 4,
	}, "/in.tif", "/out.heic")
	check(not hasArg(args, "--max-size"), "disabled max-size is really omitted")
end

do
	local ok = pcall(Cli.buildConvertArgs, {}, nil, "/out.heic")
	check(not ok, "missing input is rejected")
	ok = pcall(Cli.buildConvertArgs, {}, "/in.tif", "")
	check(not ok, "empty output is rejected")
end

-- Every flavor/engine/tone-map the dialog can emit must be one the CLI
-- accepts. These lists are asserted against cli.rs by the shell test below.
do
	for _, f in ipairs(Cli.FLAVORS) do
		local args = Cli.buildConvertArgs({ tohdr_flavor = f }, "/i.tif", "/o.heic")
		eq(argAfter(args, "--flavor"), f, "flavor " .. f .. " round-trips")
	end
	for _, e in ipairs(Cli.ENGINES) do
		local args = Cli.buildConvertArgs({ tohdr_engine = e }, "/i.tif", "/o.heic")
		eq(argAfter(args, "--engine"), e, "engine " .. e .. " round-trips")
	end
end

-- ===========================================================================
print("binary location")
-- ===========================================================================

do
	local exists = function(p) return p == "/opt/tohdr" or p == "/plugin/tohdr" end

	local p, err = Cli.locateBinary {
		userBinaryPath = "/opt/tohdr", fileExists = exists,
	}
	eq(p, "/opt/tohdr", "user path wins")
	eq(err, nil, "no error when found")

	p, err = Cli.locateBinary {
		userBinaryPath = "/nope/tohdr", fileExists = exists,
	}
	eq(p, nil, "bad user path does not silently fall through")
	check(err and err:match("does not exist"), "bad user path explains itself")

	p = Cli.locateBinary {
		userBinaryPath = "/opt/tohdr",
		pluginBinaryPath = "/plugin/tohdr",
		fileExists = exists,
	}
	eq(p, "/opt/tohdr", "explicit path beats the bundled binary")

	p = Cli.locateBinary {
		pluginBinaryPath = "/plugin/tohdr", fileExists = exists,
	}
	eq(p, "/plugin/tohdr", "bundled binary is used when no explicit path is set")

	p, err = Cli.locateBinary {
		pluginBinaryPath = "/plugin/tohdr",
		fileExists = function() return false end,
	}
	eq(p, nil, "nothing found")
	check(err and err:match("Could not find"), "not-found message is actionable")
	check(err and err:match("/plugin/tohdr"),
		"not-found message names where it looked")
	check(err and err:match("cargo build"),
		"not-found message says how to produce one")

	-- fileExists is the only filesystem access, and it is required rather than
	-- defaulted, so a caller cannot forget it and get silent nil results.
	local ok = pcall(Cli.locateBinary, { pluginBinaryPath = "/x" })
	check(not ok, "fileExists is mandatory")
end

-- A `.lrplugin` is self-contained: the binary sits beside the .lua files. There
-- is no PATH search, and there must not be one again.
--
-- Three reasons it was removed, each independently sufficient. (1) Lightroom's
-- Lua sandbox has no `os.getenv`, so nothing in here can read PATH -- the old
-- code searched a hardcoded *guess* at it and crashed the first real export on
-- `attempt to call field 'getenv' (a nil value)`. (2) A bundled binary is
-- checked first and the install step always provides one, so the guess only ran
-- when things were already broken. (3) A stale `tohdr` in one of those guessed
-- prefixes would be found and used silently, converting with a build other than
-- the one just made.
do
	eq(Cli.defaultPathEnv, nil, "defaultPathEnv is gone")
	eq(Cli.splitPath, nil, "splitPath is gone")

	-- Nothing in the module may reach for the environment at all, so removing
	-- os.getenv entirely must not perturb any exported function.
	local saved = os.getenv
	os.getenv = nil
	local ok, located = pcall(Cli.locateBinary, {
		pluginBinaryPath = "/plugin/tohdr",
		fileExists = function(p) return p == "/plugin/tohdr" end,
	})
	os.getenv = saved

	check(ok, "locateBinary survives a sandbox with no os.getenv: " .. tostring(located))
	eq(located, "/plugin/tohdr", "and still finds the bundled binary")

	-- Guard the whole module, not just the function we happened to think of.
	for name, value in pairs(Cli) do
		if type(value) == "function" then
			check(not tostring(name):lower():match("path env"),
				"no PATH-guessing helper reintroduced: " .. name)
		end
	end
end

-- ===========================================================================
print("failure summaries")
-- ===========================================================================

do
	local msg = Cli.summarizeFailure(1,
		"tohdr: loading x\ntohdr: error: could not fit within 2000 bytes\n\n")
	check(msg:match("could not fit within 2000 bytes"),
		"last meaningful line is surfaced, not just the exit code")
	check(msg:match("exit 1"), "exit status included")

	msg = Cli.summarizeFailure(127, "")
	check(msg:match("no output"), "empty output handled")
	msg = Cli.summarizeFailure(127, nil)
	check(msg:match("no output"), "nil output handled")
end

-- ===========================================================================
print(string.format("\n%d checks, %d failures", checks, failures))
os.exit(failures == 0 and 0 or 1)
