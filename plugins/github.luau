-- github: GitHub's REST API, through gh when it is installed and signed in, else
-- over https with GITHUB_TOKEN (or GH_TOKEN, or github_token in config.json);
-- with neither, public reads still work, at GitHub's lower rate.
local API = "https://api.github.com"
local GH = "gh"

local function key(name)
	local ok, got = pcall(secret, name)
	return if ok then got else nil
end
local token = key("GITHUB_TOKEN") or key("GH_TOKEN")

-- Whether gh can answer, asked once a VM: `gh auth token` is offline, and
-- fails alike when gh is missing (127) or signed out.
local signed_in
local function gh_ready(cwd)
	if signed_in == nil then
		signed_in = sh(GH .. " auth token >/dev/null 2>&1", { cwd = cwd }).code == 0
	end
	return signed_in
end

local function quote(s)
	return "'" .. (s:gsub("'", "'\\''")) .. "'"
end

local function encode(s)
	return (tostring(s):gsub("[^%w%-%._~]", function(c)
		return string.format("%%%02X", c:byte())
	end))
end

-- A query as a string, its keys sorted so a call reads the same each time.
local function query_string(query)
	local keys = {}
	for k in query do
		table.insert(keys, k)
	end
	table.sort(keys)
	for i, k in keys do
		keys[i] = encode(k) .. "=" .. encode(query[k])
	end
	return table.concat(keys, "&")
end

local function decode(text)
	return if text:match("^%s*$") then nil else json.decode(text)
end

local function via_gh(method, path, opts)
	if opts.query and next(opts.query) then
		path ..= (if path:find("?", 1, true) then "&" else "?") .. query_string(opts.query)
	end
	local cmd = GH .. " api -X " .. method .. " " .. quote(path)
	if opts.json ~= nil then
		-- json.encode writes one line, so it can't hold the closing delimiter.
		cmd ..= " --input - <<'ACORN_JSON'\n" .. json.encode(opts.json) .. "\nACORN_JSON"
	end
	local r = sh(cmd, { cwd = opts.cwd })
	if r.code ~= 0 then
		local why = r.stderr:match("^%s*(.-)%s*$")
		error("github " .. method .. " " .. path .. ": " .. (if why ~= "" then why else r.stdout), 0)
	end
	return decode(if r.full then read(r.full) else r.stdout)
end

-- {owner} and {repo} as gh fills them: from the cwd's origin remote.
local function fill(path, cwd)
	if not path:find("{owner}", 1, true) and not path:find("{repo}", 1, true) then
		return path
	end
	local url = sh("git remote get-url origin", { cwd = cwd }).stdout
	local owner, repo = url:match("github%.com[:/]([^/]+)/([^/%s]-)%.git%s*$")
	if not owner then
		owner, repo = url:match("github%.com[:/]([^/]+)/([^/%s]+)")
	end
	if not owner then
		error("github: no GitHub origin remote in " .. (cwd or "the work dir") .. " to fill {owner} and {repo}", 0)
	end
	return (path:gsub("{owner}", owner):gsub("{repo}", repo))
end

local function via_https(method, path, opts)
	local headers = { Accept = "application/vnd.github+json", ["X-GitHub-Api-Version"] = "2022-11-28" }
	if token then
		headers.Authorization = "Bearer " .. token
	end
	local res = http({
		url = API .. "/" .. fill(path, opts.cwd),
		method = method,
		query = opts.query,
		headers = headers,
		json = opts.json,
	})
	return decode(res.text)
end

local function github(path, opts)
	opts = opts or {}
	path = path:gsub("^/+", "")
	local method = (opts.method or if opts.json ~= nil then "POST" else "GET"):upper()
	if gh_ready(opts.cwd) then
		return via_gh(method, path, opts)
	end
	return via_https(method, path, opts)
end

return {
	github = {
		doc = "github(path: string, opts?: {method: string?, query: table?, json: any?, cwd: string?}): any   -- GitHub's REST API, e.g. github('repos/{owner}/{repo}/pulls', {json = {title = t, head = branch, base = 'main'}}); {owner} and {repo} come from the origin remote in cwd; method is GET, or POST with json; the decoded reply (nil if empty); raises on an error status. Through gh when signed in, else GITHUB_TOKEN; for git itself use sh",
		value = github,
	},
}
