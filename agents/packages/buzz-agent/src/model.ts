import { createCompatModel, type Model } from "@dimension-agents/shared";
import { registerApiProvider } from "@mariozechner/pi-ai";
import { chatCompletion } from "./transport.js";

const API = "buzz-chat-completions-no-auth";

export function createChatModel(): Model<typeof API> {
    const base = process.env.MODEL_BASE_URL?.trim();
    const id = process.env.MODEL_NAME?.trim();
    if (!base || !id) throw new Error("MODEL_BASE_URL and MODEL_NAME are required; no provider fallback");
    const url = new URL(base);
    if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
        throw new Error("MODEL_BASE_URL must be an HTTP(S) URL without credentials, query, or fragment");
    }
    const path = url.pathname.replace(/\/+$/, "");
    url.pathname = path.endsWith("/v1") ? path : `${path}/v1`;

    // pi-ai's built-in OpenAI transport cannot represent an unauthenticated
    // endpoint. Register this tool-free protocol adapter only in this process.
    registerApiProvider({ api: API, stream: chatCompletion, streamSimple: chatCompletion });
    return {
        ...createCompatModel({
            id,
            name: id,
            baseUrl: url.toString().replace(/\/$/, ""),
            provider: "buzz-openai-compatible",
            apiKeyEnvVariable: "",
            reasoning: false,
            contextWindow: 32768,
            maxTokens: 2048,
        }),
        api: API,
        compat: undefined,
        headers: {},
    };
}
