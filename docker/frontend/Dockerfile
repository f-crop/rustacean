# syntax=docker/dockerfile:1

# ── Stage 1: build ────────────────────────────────────────────────────────────
FROM node:20-alpine AS builder

WORKDIR /app

# Build-time feature flag — baked into the static bundle at build time.
# Safe default is off; set to "true" via compose build.args to enable.
ARG VITE_FEATURE_CHAT_PANEL=false
ENV VITE_FEATURE_CHAT_PANEL=${VITE_FEATURE_CHAT_PANEL}

# Cache npm install layer separately from source
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci

# Copy source and build
COPY frontend/ ./
RUN npm run build

# ── Stage 2: serve static files via nginx ─────────────────────────────────────
FROM nginx:alpine AS runtime

ARG GIT_SHA=unknown
LABEL git_sha=${GIT_SHA}

COPY --from=builder /app/dist /usr/share/nginx/html
COPY docker/frontend/nginx.conf /etc/nginx/conf.d/default.conf

EXPOSE 80

CMD ["nginx", "-g", "daemon off;"]
