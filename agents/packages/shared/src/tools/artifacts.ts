import { Type } from "@sinclair/typebox";
import { AgentTool } from "@mariozechner/pi-agent-core";

const UNAVAILABLE_MSG = "Artifact storage is not available in standalone mode. Use local filesystem tools instead.";

// ── Artifact Upload ─────────────────────────────────────────────────────────

const ArtifactUploadInput = Type.Object({
    path: Type.String({ description: "Path to a file or directory to upload. If a directory, all files inside are uploaded recursively (skipping node_modules and .git)." }),
    prefix: Type.Optional(Type.String({ description: "Key prefix for uploaded files (e.g. 'my-project/'). Defaults to the directory/file name." })),
});

export const artifactUploadTool: AgentTool = {
    name: "artifact_upload",
    label: "Artifact Upload",
    description: "Upload a file or entire directory to persistent artifact storage. For directories, all files are uploaded recursively with their relative paths as keys. Useful for saving project outputs, build artifacts, generated code, etc.",
    parameters: ArtifactUploadInput,
    execute: async (_toolCallId, _params, _signal, _onUpdate) => {
        return {
            content: [{ type: "text", text: UNAVAILABLE_MSG }],
            details: { error: true },
        };
    },
};

// ── Artifact Download ───────────────────────────────────────────────────────

const ArtifactDownloadInput = Type.Object({
    key: Type.String({ description: "Storage key of the artifact to download" }),
    file_path: Type.String({ description: "Absolute path where the file should be written" }),
});

export const artifactDownloadTool: AgentTool = {
    name: "artifact_download",
    label: "Artifact Download",
    description: "Download an artifact from persistent storage and write it to a local file path. Use the key returned from a previous artifact_upload or found via artifact_list.",
    parameters: ArtifactDownloadInput,
    execute: async (_toolCallId, _params, _signal, _onUpdate) => {
        return {
            content: [{ type: "text", text: UNAVAILABLE_MSG }],
            details: { error: true },
        };
    },
};

// ── Artifact List ───────────────────────────────────────────────────────────

const ArtifactListInput = Type.Object({
    prefix: Type.Optional(Type.String({ description: "Filter artifacts by key prefix" })),
});

export const artifactListTool: AgentTool = {
    name: "artifact_list",
    label: "Artifact List",
    description: "List artifacts in persistent storage. Optionally filter by a key prefix. Returns artifact keys and sizes.",
    parameters: ArtifactListInput,
    execute: async (_toolCallId, _params, _signal, _onUpdate) => {
        return {
            content: [{ type: "text", text: UNAVAILABLE_MSG }],
            details: { error: true },
        };
    },
};
