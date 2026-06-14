import type { Model } from "@mariozechner/pi-ai";

interface CompatModelOptions {
    /** Model ID as sent to the API. */
    id: string;
    /** Human-readable name. */
    name: string;
    /** API base URL (e.g., "https://api.moonshot.cn/v1"). */
    baseUrl: string;
    /** Provider identifier. */
    provider: string;
    /** Env variable name for the API key. */
    apiKeyEnvVariable: string;
    /** Whether the model supports reasoning/thinking. Default: false. */
    reasoning?: boolean;
    /** Input modalities. Default: ["text"]. */
    input?: ("text" | "image")[];
    /** Context window size. Default: 128000. */
    contextWindow?: number;
    /** Max output tokens. Default: 16384. */
    maxTokens?: number;
}

/**
 * Create a Model object for an OpenAI-compatible API provider (e.g., Kimi/Moonshot).
 * The API key is read from the specified environment variable and set as an Authorization header.
 */
export function createCompatModel(options: CompatModelOptions): Model<"openai-completions"> {
    const apiKey = process.env[options.apiKeyEnvVariable] ?? "";

    return {
        id: options.id,
        name: options.name,
        api: "openai-completions",
        provider: options.provider,
        baseUrl: options.baseUrl,
        reasoning: options.reasoning ?? false,
        input: options.input ?? ["text"],
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
        contextWindow: options.contextWindow ?? 128000,
        maxTokens: options.maxTokens ?? 16384,
        headers: {
            Authorization: `Bearer ${apiKey}`,
        },
    };
}
