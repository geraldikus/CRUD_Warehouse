FROM python:3.12-slim

# libgomp1: onnxruntime (Piper and Silero). libatomic1: vosk libvosk.so on aarch64.
# ca-certificates: the first start downloads the models over HTTPS.
RUN apt-get update \
    && apt-get install -y --no-install-recommends libgomp1 libatomic1 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt
COPY app ./app

ENV PYTHONUNBUFFERED=1
ENV MODELS_DIR=/models
EXPOSE 8000

# bootstrap downloads missing models, then replaces itself with uvicorn.
ENTRYPOINT ["python", "-m", "app.bootstrap"]
