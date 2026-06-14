import { Type } from "@sinclair/typebox";
import { Value } from "@sinclair/typebox/value";
import { AgentTool } from "@mariozechner/pi-agent-core";

const FIRECRAWL_API_KEY = process.env["FIRECRAWL_API_KEY"] ?? "";
const FIRECRAWL_BASE = process.env["FIRECRAWL_BASE_URL"] ?? "https://api.firecrawl.dev";

// ── Web Search ───────────────────────────────────────────────────────────────

const WebSearchInput = Type.Object({
    query: Type.String({ description: "Search query" }),
    limit: Type.Optional(Type.Number({ description: "Max results (default: 5)" })),
});

export const webSearchTool: AgentTool = {
    name: "web_search",
    label: "Web Search",
    description: "Search the web using Firecrawl. Returns URLs, titles, and content snippets. Use for finding documentation, researching APIs, looking up error messages, etc.",
    parameters: WebSearchInput,
    execute: async (_toolCallId, params, _signal, _onUpdate) => {
        const p = Value.Parse(WebSearchInput, params);

        if (!FIRECRAWL_API_KEY) {
            return {
                content: [{ type: "text", text: "Error: FIRECRAWL_API_KEY not set. Web search unavailable." }],
                details: { error: true },
            };
        }

        try {
            const response = await fetch(`${FIRECRAWL_BASE}/v1/search`, {
                method: "POST",
                headers: {
                    "Content-Type": "application/json",
                    "Authorization": `Bearer ${FIRECRAWL_API_KEY}`,
                },
                body: JSON.stringify({
                    query: p.query,
                    limit: p.limit ?? 5,
                    scrapeOptions: { formats: ["markdown"] },
                }),
            });

            if (!response.ok) {
                const text = await response.text();
                return {
                    content: [{ type: "text", text: `Search failed (${response.status}): ${text}` }],
                    details: { error: true },
                };
            }

            const data = await response.json() as { success: boolean; data: Array<{ url: string; title: string; markdown?: string; description?: string }> };

            if (!data.success || !data.data?.length) {
                return {
                    content: [{ type: "text", text: "No results found." }],
                    details: {},
                };
            }

            const results = data.data.map((r, i) => {
                const snippet = r.markdown?.slice(0, 500) ?? r.description ?? "";
                return `${i + 1}. **${r.title ?? "Untitled"}**\n   ${r.url}\n   ${snippet}\n`;
            }).join("\n");

            return {
                content: [{ type: "text", text: results }],
                details: { count: data.data.length },
            };
        } catch (err) {
            return {
                content: [{ type: "text", text: `Search error: ${err instanceof Error ? err.message : String(err)}` }],
                details: { error: true },
            };
        }
    },
};

// ── Web Fetch ────────────────────────────────────────────────────────────────

const WebFetchInput = Type.Object({
    url: Type.String({ description: "URL to fetch" }),
    max_chars: Type.Optional(Type.Number({ description: "Max characters to return (default: 10000)" })),
});

export const webFetchTool: AgentTool = {
    name: "web_fetch",
    label: "Web Fetch",
    description: "Fetch a URL and extract readable content as markdown. Use to read documentation pages, API references, READMEs, etc. Uses Firecrawl for JS-heavy sites.",
    parameters: WebFetchInput,
    execute: async (_toolCallId, params, _signal, _onUpdate) => {
        const p = Value.Parse(WebFetchInput, params);
        const maxChars = p.max_chars ?? 10000;

        // Try Firecrawl first if available
        if (FIRECRAWL_API_KEY) {
            try {
                const response = await fetch(`${FIRECRAWL_BASE}/v1/scrape`, {
                    method: "POST",
                    headers: {
                        "Content-Type": "application/json",
                        "Authorization": `Bearer ${FIRECRAWL_API_KEY}`,
                    },
                    body: JSON.stringify({
                        url: p.url,
                        formats: ["markdown"],
                        onlyMainContent: true,
                    }),
                });

                if (response.ok) {
                    const data = await response.json() as { success: boolean; data: { markdown?: string; title?: string } };
                    if (data.success && data.data?.markdown) {
                        let content = data.data.markdown;
                        if (content.length > maxChars) {
                            content = content.slice(0, maxChars) + "\n\n... (truncated)";
                        }
                        return {
                            content: [{ type: "text", text: content }],
                            details: { url: p.url, title: data.data.title, extractor: "firecrawl" },
                        };
                    }
                }
            } catch {
                // Fall through to plain fetch
            }
        }

        // Fallback: plain HTTP fetch
        try {
            const response = await fetch(p.url, {
                headers: { "User-Agent": "dimension-coder/0.1" },
            });
            let text = await response.text();
            if (text.length > maxChars) {
                text = text.slice(0, maxChars) + "\n\n... (truncated)";
            }
            return {
                content: [{ type: "text", text }],
                details: { url: p.url, extractor: "plain" },
            };
        } catch (err) {
            return {
                content: [{ type: "text", text: `Fetch error: ${err instanceof Error ? err.message : String(err)}` }],
                details: { error: true },
            };
        }
    },
};
