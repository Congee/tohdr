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
local function arg_after(args, flag)
	for i, a in ipairs(args) do
		if a == flag then return args[i + 1] end
	end
	return nil
end

local function has_arg(args, flag)
	for _, a in ipairs(args) do
		if a == flag then return true end
	end
	return false
end

-- ===========================================================================
print("quoting")
-- ===========================================================================

eq(Cli.quote_arg("plain"), "'plain'", "plain string")
eq(Cli.quote_arg("with space.tif"), "'with space.tif'", "spaces")
eq(Cli.quote_arg("/a/b c/d.tif"), "'/a/b c/d.tif'", "path with a space")
-- The dangerous ones: shell metacharacters must stay literal.
eq(Cli.quote_arg("a$(rm -rf /)b"), "'a$(rm -rf /)b'", "command substitution")
eq(Cli.quote_arg("a`whoami`b"), "'a`whoami`b'", "backticks")
eq(Cli.quote_arg('a"b'), [['a"b']], "double quote")
eq(Cli.quote_arg("a;b"), "'a;b'", "semicolon")
eq(Cli.quote_arg("a\\b"), "'a\\b'", "backslash")
eq(Cli.quote_arg("naïve — 日本語.tif"), "'naïve — 日本語.tif'", "unicode")
-- A single quote is the one character that must break out and come back in.
eq(Cli.quote_arg("it's"), [['it'\''s']], "embedded single quote")
eq(Cli.quote_arg("''"), [[''\'''\''']], "two single quotes")

do
	local cmd = Cli.build_command_line("/usr/local/bin/to hdr",
		{ "convert", "/in put.tif", "--output", "/out'put.heic" })
	eq(cmd, [['/usr/local/bin/to hdr' 'convert' '/in put.tif' '--output' '/out'\''put.heic']],
		"full command line quotes every element")
end

-- ===========================================================================
print("convert args")
-- ===========================================================================

do
	local args = Cli.build_convert_args({
		tohdr_flavor = "both",
		tohdr_engine = "portable",
		tohdr_quality = 85,
		tohdr_minQuality = 40,
		tohdr_toneMap = "reinhard",
	}, "/in.tif", "/out.heic")

	eq(args[1], "convert", "subcommand first")
	eq(args[2], "/in.tif", "input is positional")
	eq(arg_after(args, "--output"), "/out.heic", "output flag")
	eq(arg_after(args, "--flavor"), "both", "flavor")
	eq(arg_after(args, "--engine"), "portable", "engine")
	eq(arg_after(args, "--quality"), "85", "quality stringified")
	eq(arg_after(args, "--tone-map"), "reinhard", "tone map")
	check(has_arg(args, "--json"), "--json requested so failures are parseable")
	-- ExportServiceProvider asks Lightroom for a `p3_hdr` intermediate; this is
	-- the same decision spelled to the CLI, and the two must not drift.
	eq(arg_after(args, "--colour-space"), "p3", "colour space stated explicitly")
	check(not has_arg(args, "--max-size"), "no --max-size when disabled")
	check(not has_arg(args, "--headroom"), "no --headroom unless set")
end

do
	local args = Cli.build_convert_args({
		tohdr_maxSizeEnabled = true,
		tohdr_maxSizeValue = 4,
		tohdr_maxSizeUnit = "MB",
	}, "/in.tif", "/out.heic")
	eq(arg_after(args, "--max-size"), "4MB", "max size composes value and unit")
end

do
	local args = Cli.build_convert_args({
		tohdr_maxSizeEnabled = true,
		tohdr_maxSizeValue = 4,
		tohdr_maxSizeUnit = "MiB",
	}, "/in.tif", "/out.heic")
	eq(arg_after(args, "--max-size"), "4MiB", "MiB unit preserved")
end

do
	-- tohdr_maxSizeEnabled false must suppress the flag even with a value present,
	-- or unchecking the box in the dialog would do nothing.
	local args = Cli.build_convert_args({
		tohdr_maxSizeEnabled = false,
		tohdr_maxSizeValue = 4,
	}, "/in.tif", "/out.heic")
	check(not has_arg(args, "--max-size"), "disabled max-size is really omitted")
end

do
	local ok = pcall(Cli.build_convert_args, {}, nil, "/out.heic")
	check(not ok, "missing input is rejected")
	ok = pcall(Cli.build_convert_args, {}, "/in.tif", "")
	check(not ok, "empty output is rejected")
end

-- Every flavor/engine/tone-map the dialog can emit must be one the CLI
-- accepts. These lists are asserted against cli.rs by the shell test below.
do
	for _, f in ipairs(Cli.FLAVORS) do
		local args = Cli.build_convert_args({ tohdr_flavor = f }, "/i.tif", "/o.heic")
		eq(arg_after(args, "--flavor"), f, "flavor " .. f .. " round-trips")
	end
	for _, e in ipairs(Cli.ENGINES) do
		local args = Cli.build_convert_args({ tohdr_engine = e }, "/i.tif", "/o.heic")
		eq(arg_after(args, "--engine"), e, "engine " .. e .. " round-trips")
	end
end

-- ===========================================================================
print("maker note from the original file")
-- ===========================================================================

do
	local raw = "/Volumes/Photos/7.19/DSC07746.ARW"
	local args = Cli.build_convert_args({ tohdr_makerNote = true }, "/in.tif", "/out.heic", raw)
	eq(arg_after(args, "--maker-note-from"), raw, "the raw path is passed through")
end

do
	-- Both halves are required, and each has a distinct cause: the checkbox is
	-- the user's choice, the path is whether we found a file at all. Either
	-- missing must produce the same command line as before this existed.
	local plain = Cli.build_convert_args({}, "/in.tif", "/out.heic")

	local unchecked = Cli.build_convert_args(
		{ tohdr_makerNote = false }, "/in.tif", "/out.heic", "/some/DSC07746.ARW"
	)
	check(not has_arg(unchecked, "--maker-note-from"), "unchecked really omits the flag")
	eq(#unchecked, #plain, "unchecked emits nothing extra")

	for _, missing in ipairs({ "nil", "" }) do
		local path = (missing == "") and "" or nil
		local args = Cli.build_convert_args(
			{ tohdr_makerNote = true }, "/in.tif", "/out.heic", path
		)
		check(not has_arg(args, "--maker-note-from"),
			"no flag when the path is " .. missing)
		eq(#args, #plain, "nothing extra when the path is " .. missing)
	end
end

do
	-- A raw path with a space and a quote in it, since it is a filename we did
	-- not choose and it reaches a shell. The whole point of quote_arg.
	local raw = "/Volumes/My Photos/it's a raw.ARW"
	local args = Cli.build_convert_args({ tohdr_makerNote = true }, "/in.tif", "/out.heic", raw)
	local cmd = Cli.build_command_line("/bin/tohdr", args)
	check(cmd:find("'/Volumes/My Photos/it'\\''s a raw.ARW'", 1, true) ~= nil,
		"the raw path is quoted, quote and all")
end

-- ===========================================================================
print("gain map source")
-- ===========================================================================

do
	-- The shape that actually arrives. Captured from a real `--json` run, which is
	-- the only form the plugin ever sees: with `--json` the CLI prints this object
	-- and no prose at all, which is why reading the prose was a gate that could
	-- never close.
	local json = '{"input":"/tmp/in.tif","output":"/tmp/out.heic","engine":"apple-imageio",'
		.. '"flavor":"both","tone_map":"reinhard","gain_map_source":"derived",'
		.. '"exif_source":"tiff-ifd0","exif_tags":52}'
	eq(Cli.gain_map_source(json), "derived", "derived is read out of the JSON")

	local ok_json = json:gsub('"derived"', '"lightroom-embedded"')
	eq(Cli.gain_map_source(ok_json), "lightroom-embedded",
		"a transcoded gain map is read out of the JSON")
end

do
	-- The prose forms, so a run without --json is not a blind spot.
	eq(Cli.gain_map_source("  gain map: derived from the source's HDR pixels"),
		"derived", "the text form is still recognized")
	eq(Cli.gain_map_source("  gain map: transcoded from the source's own, not derived"),
		"lightroom-embedded", "the transcoded text form is recognized")
end

do
	-- Nothing to go on must read as nil, not as "derived": the caller deletes the
	-- output and fails the photo on "derived", so guessing it would throw away a
	-- good file every time an older binary said nothing.
	eq(Cli.gain_map_source(""), nil, "empty output yields nil")
	eq(Cli.gain_map_source(nil), nil, "nil output yields nil")
	eq(Cli.gain_map_source("wrote /tmp/out.heic (123 bytes)"), nil,
		"an output that names no source yields nil")
	-- And a filename cannot fake it, since the key has to be there too.
	eq(Cli.gain_map_source('wrote "/photos/gain map: derived.heic"'), nil,
		"prose inside a quoted filename is not the JSON field")
end

-- ===========================================================================
print("binary location")
-- ===========================================================================

do
	local exists = function(p) return p == "/opt/tohdr" or p == "/plugin/tohdr" end

	local p, err = Cli.locate_binary {
		user_binary_path = "/opt/tohdr", file_exists = exists,
	}
	eq(p, "/opt/tohdr", "user path wins")
	eq(err, nil, "no error when found")

	p, err = Cli.locate_binary {
		user_binary_path = "/nope/tohdr", file_exists = exists,
	}
	eq(p, nil, "bad user path does not silently fall through")
	check(err and err:match("does not exist"), "bad user path explains itself")

	p = Cli.locate_binary {
		user_binary_path = "/opt/tohdr",
		plugin_binary_path = "/plugin/tohdr",
		file_exists = exists,
	}
	eq(p, "/opt/tohdr", "explicit path beats the bundled binary")

	p = Cli.locate_binary {
		plugin_binary_path = "/plugin/tohdr", file_exists = exists,
	}
	eq(p, "/plugin/tohdr", "bundled binary is used when no explicit path is set")

	p, err = Cli.locate_binary {
		plugin_binary_path = "/plugin/tohdr",
		file_exists = function() return false end,
	}
	eq(p, nil, "nothing found")
	check(err and err:match("Could not find"), "not-found message is actionable")
	check(err and err:match("/plugin/tohdr"),
		"not-found message names where it looked")
	check(err and err:match("cargo build"),
		"not-found message says how to produce one")

	-- file_exists is the only filesystem access, and it is required rather than
	-- defaulted, so a caller cannot forget it and get silent nil results.
	local ok = pcall(Cli.locate_binary, { plugin_binary_path = "/x" })
	check(not ok, "file_exists is mandatory")
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
	local ok, located = pcall(Cli.locate_binary, {
		plugin_binary_path = "/plugin/tohdr",
		file_exists = function(p) return p == "/plugin/tohdr" end,
	})
	os.getenv = saved

	check(ok, "locate_binary survives a sandbox with no os.getenv: " .. tostring(located))
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

-- LrTasks.execute returns the OS shell's wait status, not a bare exit code, so
-- on macOS `tohdr` exiting 1 arrives as 256. The first real export reported
-- "tohdr failed (exit 256)" -- a number that appears nowhere in the CLI.
do
	eq(Cli.decode_exit_status(256), 1, "256 is exit 1")
	eq(Cli.decode_exit_status(512), 2, "512 is exit 2")
	eq(Cli.decode_exit_status(0), 0, "success is untouched")
	eq(Cli.decode_exit_status(1), 1, "a plain code (Windows) is untouched")
	-- 128+signal is the familiar shell rendering; leave it legible.
	eq(Cli.decode_exit_status(139), 139, "signal death passes through")
	eq(Cli.decode_exit_status(nil), nil, "nil survives")

	local msg = Cli.summarize_failure(256, "tohdr: error: could not fit\n")
	check(msg:match("exit 1"), "summary reports the real exit code")
	check(not msg:match("256"), "and never shows the raw wait status")
end

do
	local msg = Cli.summarize_failure(1,
		"tohdr: loading x\ntohdr: error: could not fit within 2000 bytes\n\n")
	check(msg:match("could not fit within 2000 bytes"),
		"last meaningful line is surfaced, not just the exit code")
	check(msg:match("exit 1"), "exit status included")

	msg = Cli.summarize_failure(127, "")
	check(msg:match("no output"), "empty output handled")
	msg = Cli.summarize_failure(127, nil)
	check(msg:match("no output"), "nil output handled")
end

-- ===========================================================================
-- advisories: what a *successful* run said about how it succeeded.
-- ===========================================================================
print("success advisories")

do
	local out = table.concat({
		"tohdr: loading /tmp/x.tiff",
		"tohdr: note: /tmp/x.tiff is srgb by its own ICC profile, so the output declares srgb",
		"tohdr: wrote /tmp/x.heic (srgb base)",
	}, "\n")
	local list = Cli.advisories(out)
	eq(#list, 1, "one advisory found in a successful run")
	check(list[1].text:match("^note:"), "the tohdr: prefix is stripped, the kind is kept")
	check(list[1].text:match("is srgb by its own ICC profile"),
		"the colour-space report survives intact")
	eq(list[1].count, 1, "counted once")

	-- The case this exists for: silence when the request was honoured.
	eq(#Cli.advisories("tohdr: loading x\ntohdr: wrote y (p3 base)\n"), 0,
		"a run with nothing to say produces no advisories")
	eq(#Cli.advisories(""), 0, "empty output is silent")
	eq(#Cli.advisories(nil), 0, "nil output is silent")
	eq(Cli.summarize_advisories({}), nil, "no advisories means no dialog at all")
	eq(Cli.summarize_advisories(nil), nil, "nil list means no dialog at all")
end

do
	-- A filename or Exif string containing the word must not fake an advisory;
	-- only the CLI's own line prefix counts.
	local list = Cli.advisories(table.concat({
		"tohdr: loading /tmp/note: not really.tiff",
		"note: this line has no tohdr prefix",
		"tohdr: warning: /tmp/x.tiff embeds no ICC profile this build recognises",
	}, "\n"))
	eq(#list, 1, "only prefixed advisory lines match")
	check(list[1].text:match("^warning:"), "and it is the warning")
end

do
	-- 200 photos hitting the same condition is one line with a count, not 200.
	local one = Cli.advisories("tohdr: note: dropped the carried MakerApple headroom tags\n")
	local all = {}
	for _ = 1, 3 do
		Cli.merge_advisories(all, one)
	end
	eq(#all, 1, "the same advisory from many photos collapses to one entry")
	eq(all[1].count, 3, "with a count of how many photos hit it")
	local msg = Cli.summarize_advisories(all) or ""
	check(msg:match("3 photos"), "the count reaches the message")
	check(msg:match("succeeded"), "and the message says the conversion still succeeded")

	Cli.merge_advisories(all, Cli.advisories("tohdr: warning: something else entirely\n"))
	eq(#all, 2, "a different advisory is kept separately")
	check((Cli.summarize_advisories(all) or ""):match("something else entirely"), "and is listed")
	-- Merging a run that said nothing must not invent an entry.
	Cli.merge_advisories(all, Cli.advisories("tohdr: wrote /tmp/x.heic (p3 base)"))
	eq(#all, 2, "a silent run adds nothing")
end

-- ===========================================================================
print("export preset keys are frozen")
-- ===========================================================================
--
-- These strings are not ours to rename. Lightroom writes them into every saved
-- export preset, so a renamed key is simply not found on load: the setting
-- reverts to its default, silently, taking the user's custom binary path with
-- it, and nothing anywhere reports that it happened. That is what a snake_case
-- sweep over this plugin did do, which is why the list now lives in a test
-- instead of in a habit. Adding a key is fine -- add it here too, deliberately.
--
-- Read as text rather than loaded: the two files import LrView and friends, so
-- a stock interpreter cannot run them, and matching source is enough to catch
-- the rename this guards against.
do
	local frozen = {
		'tohdr_flavor', 'tohdr_engine', 'tohdr_maxSizeEnabled',
		'tohdr_maxSizeValue', 'tohdr_maxSizeUnit', 'tohdr_quality',
		'tohdr_minQuality', 'tohdr_toneMap', 'tohdr_gainSubsample',
		'tohdr_makerNote', 'tohdr_binaryPath',
	}

	local function slurp(path)
		local f = assert(io.open(path, 'r'), "cannot read " .. path)
		local text = f:read('*a')
		f:close()
		return text
	end

	local provider = slurp('lightroom/tohdr.lrplugin/ExportServiceProvider.lua')
	local dialog = slurp('lightroom/tohdr.lrplugin/TohdrExportDialog.lua')

	local expected, declared = {}, {}
	for key in provider:gmatch("key%s*=%s*'(tohdr_[%w_]+)'") do declared[key] = true end

	for _, key in ipairs(frozen) do
		expected[key] = true
		check(declared[key], key .. " is still declared in exportPresetFields")
	end
	for key in pairs(declared) do
		check(expected[key], key .. " is declared but is not in the frozen list")
	end

	-- Both halves of the dialog, for the same reason in opposite directions: a
	-- default under a misspelled key never reaches the widget bound to the real
	-- one, and a binding to a misspelled key shows an empty control. Neither
	-- errors, so both read as a Lightroom bug rather than a typo here.
	for key in dialog:gmatch("(tohdr_[%w_]+)%s*=") do
		check(expected[key], "dialog default " .. key .. " names a real key")
	end
	for key in dialog:gmatch("bind%s*'(tohdr_[%w_]+)'") do
		check(expected[key], "dialog binding " .. key .. " names a real key")
	end
end

-- ===========================================================================
print(string.format("\n%d checks, %d failures", checks, failures))
os.exit(failures == 0 and 0 or 1)
