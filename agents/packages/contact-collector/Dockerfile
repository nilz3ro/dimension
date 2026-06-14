FROM node:22-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*

RUN printf "nameserver 8.8.8.8\nnameserver 1.1.1.1\n" > /etc/resolv.conf.vm

RUN corepack enable && corepack prepare pnpm@latest --activate

WORKDIR /app

# Copy workspace root
COPY package.json pnpm-workspace.yaml pnpm-lock.yaml ./
COPY packages/shared/package.json packages/shared/
COPY packages/contact-collector/package.json packages/contact-collector/

RUN pnpm install --frozen-lockfile

COPY packages/shared/ packages/shared/
COPY packages/contact-collector/ packages/contact-collector/

RUN pnpm -r build

COPY packages/contact-collector/entrypoint.sh /app/entrypoint.sh
COPY packages/contact-collector/dimension.toml /etc/hyphae/dimension.toml
RUN chmod +x /app/entrypoint.sh

ENV VLLM_BASE_URL=http://100.104.71.102:8000
ENV VLLM_MODEL=openai/gpt-oss-20b
ENV VLLM_API_KEY=none

ENTRYPOINT ["/app/entrypoint.sh"]
