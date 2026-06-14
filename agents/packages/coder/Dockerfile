FROM node:22-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends git grep curl \
    && rm -rf /var/lib/apt/lists/*

RUN printf "nameserver 8.8.8.8\nnameserver 1.1.1.1\n" > /etc/resolv.conf.vm

RUN corepack enable && corepack prepare pnpm@latest --activate

WORKDIR /app

# Copy workspace root
COPY package.json pnpm-workspace.yaml pnpm-lock.yaml ./
COPY packages/shared/package.json packages/shared/
COPY packages/coder/package.json packages/coder/

RUN pnpm install --frozen-lockfile

COPY packages/shared/ packages/shared/
COPY packages/coder/ packages/coder/

RUN pnpm -r build

COPY packages/coder/entrypoint.sh /app/entrypoint.sh
COPY packages/coder/dimension.toml /etc/hyphae/dimension.toml
RUN chmod +x /app/entrypoint.sh

RUN mkdir -p /app/workspace
ENV WORKSPACE_DIR=/app/workspace
# OPENAI_API_KEY and FIRECRAWL_API_KEY are injected at runtime
# via /etc/hyphae/env (sourced by entrypoint.sh), not baked into the image.

ENTRYPOINT ["/app/entrypoint.sh"]
