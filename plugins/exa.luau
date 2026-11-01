-- web_search and web_fetch through Exa. Key: EXA_API_KEY, or exa_api_key in config.json.
local API = "https://api.exa.ai"
local key = secret("EXA_API_KEY")

local function post(path, body)
	return http({
		url = API .. path,
		headers = { ["x-api-key"] = key },
		json = body,
	}).json
end

local function search(query, opts)
	local count = math.clamp(if opts and opts.count then opts.count else 10, 1, 100)
	local got = post("/search", { query = query, numResults = count, contents = { highlights = true } })
	local blocks = {}
	for i, r in got.results or {} do
		local date = if r.publishedDate then " (" .. r.publishedDate:sub(1, 10) .. ")" else ""
		local text = table.concat(r.highlights or {}, " … ")
		blocks[i] = i .. ". " .. (r.title or "(no title)") .. date .. "\n" .. r.url .. "\n" .. text
	end
	return if #blocks == 0 then "No results." else table.concat(blocks, "\n\n")
end

local function fetch(url)
	local got = post("/contents", { urls = { url }, text = true })
	for _, s in got.statuses or {} do
		if s.status == "error" then
			error("fetch " .. url .. ": " .. (s.error and s.error.tag or "failed"), 0)
		end
	end
	local page = got.results and got.results[1]
	if not page then
		error("fetch " .. url .. ": no content", 0)
	end
	return "# " .. (page.title or "(no title)") .. "\n" .. url .. "\n\n" .. (page.text or "")
end

return {
	web_search = {
		doc = "web_search(query: string, opts?: {count: number?}): string   -- each result's title, url, date and passages that fit, best first, by Exa's meaning-based search: ask in plain words. count: 1 to 100 (10 by default)",
		value = search,
	},
	web_fetch = {
		doc = "web_fetch(url: string): string   -- a page's title and whole text, by Exa: print a slice of a long one",
		value = fetch,
	},
}
