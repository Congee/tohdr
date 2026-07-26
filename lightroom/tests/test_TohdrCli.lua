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
print("PATH splitting and binary location")
-- ===========================================================================

do
	local d = Cli.splitPath("/usr/bin:/bin")
	eq(#d, 2, "two entries")
	eq(d[1], "/usr/bin", "first entry")
	-- An empty PATH element means "current directory" in POSIX; executing a
	-- `tohdr` out of the cwd is not something we ever want to do silently.
	local e = Cli.splitPath("/usr/bin::/bin:")
	eq(#e, 2, "empty entries dropped")
	eq(#Cli.splitPath(""), 0, "empty PATH")
	eq(#Cli.splitPath(nil), 0, "nil PATH")
end

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
		pluginBinaryPath = "/plugin/tohdr",
		pathDirs = { "/usr/bin" },
		fileExists = exists,
	}
	eq(p, "/plugin/tohdr", "bundled binary beats PATH")

	p = Cli.locateBinary {
		pathDirs = { "/usr/bin", "/opt" },
		fileExists = exists,
		joinPath = function(d, n) return d .. "/" .. n end,
	}
	eq(p, "/opt/tohdr", "found on PATH")

	p, err = Cli.locateBinary {
		pathDirs = { "/usr/bin" },
		fileExists = function() return false end,
	}
	eq(p, nil, "nothing found")
	check(err and err:match("Could not find"), "not-found message is actionable")
end

do
	local env = Cli.defaultPathEnv()
	check(env:match("/usr/local/bin"), "default PATH includes /usr/local/bin")
	check(env:match("/opt/homebrew/bin"), "default PATH includes Homebrew arm64")
	for _, d in ipairs(Cli.splitPath(env)) do
		check(d ~= "/.cargo/bin" and d ~= "/.nix-profile/bin",
			"no HOME-less junk entry: " .. d)
	end
end

-- Lightroom Classic's Lua sandbox provides no `os.getenv`. Calling it there
-- raised `attempt to call field 'getenv' (a nil value)` and killed the export
-- with an "Unable to Export" dialog. Simulate that sandbox exactly: nothing in
-- TohdrCli may touch os.getenv without checking it exists first.
do
	local saved = os.getenv
	os.getenv = nil

	local ok, envOrErr = pcall(Cli.defaultPathEnv)
	os.getenv = saved

	check(ok, "defaultPathEnv survives a sandbox with no os.getenv: "
		.. tostring(envOrErr))
	if ok then
		local dirs = Cli.splitPath(envOrErr)
		check(#dirs > 0, "still yields candidate dirs without any environment")
		for _, d in ipairs(dirs) do
			check(d ~= "/.cargo/bin" and d ~= "/.nix-profile/bin",
				"no HOME-less junk entry with getenv gone: " .. d)
		end
		check(envOrErr:match("/opt/homebrew/bin"),
			"fixed prefixes survive without an environment")
	end
end

-- What the plugin actually does inside Lightroom: pass `home` from
-- LrPathUtils.getStandardFilePath('home') and no inherited PATH at all.
do
	local env = Cli.defaultPathEnv { home = "/Users/someone" }
	check(env:match("/Users/someone/%.cargo/bin"), "injected home builds cargo dir")
	check(env:match("/Users/someone/%.nix%-profile/bin"), "injected home builds nix dir")

	local dirs = Cli.splitPath(Cli.defaultPathEnv {
		inheritedPath = "/first:/second", home = "/h",
	})
	eq(dirs[1], "/first", "inherited PATH keeps precedence")
	eq(dirs[2], "/second", "inherited PATH order preserved")
	check(dirs[3] == "/opt/homebrew/bin", "appended prefixes follow the inherited PATH")

	-- An empty-string home must not synthesise "/.cargo/bin" either.
	for _, d in ipairs(Cli.splitPath(Cli.defaultPathEnv { home = "" })) do
		check(d ~= "/.cargo/bin" and d ~= "/.nix-profile/bin",
			"empty home synthesises nothing: " .. d)
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
