-- web_search and web_fetch through TinyFish. Key: TINYFISH_API_KEY, or tinyfish_api_key in config.json.
local SEARCH = "https://api.search.tinyfish.ai"
local FETCH = "https://api.fetch.tinyfish.ai"
local key = secret("TINYFISH_API_KEY")
local headers = { ["X-API-Key"] = key }

local function search(query, opts)
	local count = if opts and opts.count then opts.count else 10
	local got = http({ url = SEARCH, query = { query = query }, headers = headers }).json
	local blocks = {}
	for i, r in got.results or {} do
		if i > count then
			break
		end
		local site = if r.site_name then " (" .. r.site_name .. ")" else ""
		blocks[i] = i .. ". " .. (r.title or "(no title)") .. site .. "\n" .. r.url .. "\n" .. (r.snippet or "")
	end
	return if #blocks == 0 then "No results." else table.concat(blocks, "\n\n")
end

local function fetch(url)
	local got = http({ url = FETCH, headers = headers, json = { urls = { url }, format = "markdown" } }).json
	local failed = got.errors and got.errors[1]
	if failed then
		error("fetch " .. url .. ": " .. tostring(failed.error), 0)
	end
	local page = got.results and got.results[1]
	if not page then
		error("fetch " .. url .. ": no content", 0)
	end
	return "# " .. (page.title or "(no title)") .. "\n" .. url .. "\n\n" .. (page.text or "")
end

return {
	web_search = {
		doc = "web_search(query: string, opts?: {count: number?}): string   -- each result's title, url, site and snippet, best first, by TinyFish. count: how many to show (10 by default)",
		value = search,
	},
	web_fetch = {
		doc = "web_fetch(url: string): string   -- a page's title and whole content as Markdown, by TinyFish: print a slice of a long one",
		value = fetch,
	},
}
