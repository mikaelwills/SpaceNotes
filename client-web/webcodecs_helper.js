window.SpaceNotesCodecs = {
  _encoder: null,
  _decoder: null,
  _decoderCanvas: null,
  _onEncodedFrame: null,
  _frameSeq: 0,
  _keyframeInterval: 20,
  _mediaStream: null,
  _videoTrack: null,
  _captureInterval: null,
  _videoEl: null,
  _captureCanvas: null,

  startEncoder: function(width, height, fps, keyframeInterval, onFrame) {
    this._onEncodedFrame = onFrame;
    this._frameSeq = 0;
    this._keyframeInterval = keyframeInterval || fps;

    this._sps = null;
    this._pps = null;

    this._encoder = new VideoEncoder({
      output: (chunk, metadata) => {
        const isKey = chunk.type === 'key';

        if (metadata && metadata.decoderConfig && metadata.decoderConfig.description) {
          const desc = new Uint8Array(metadata.decoderConfig.description);
          this._parseAVCDecoderConfig(desc);
        }

        const avccData = new Uint8Array(chunk.byteLength);
        chunk.copyTo(avccData);

        let paramData = new Uint8Array(0);
        if (isKey && this._sps && this._pps) {
          const spsLen = this._sps.length;
          const ppsLen = this._pps.length;
          paramData = new Uint8Array(2 + spsLen + 2 + ppsLen);
          paramData[0] = (spsLen >> 8) & 0xFF;
          paramData[1] = spsLen & 0xFF;
          paramData.set(this._sps, 2);
          paramData[2 + spsLen] = (ppsLen >> 8) & 0xFF;
          paramData[2 + spsLen + 1] = ppsLen & 0xFF;
          paramData.set(this._pps, 2 + spsLen + 2);
        }

        const paramSize = paramData.length;
        const output = new Uint8Array(4 + paramSize + avccData.length);
        output[0] = paramSize & 0xFF;
        output[1] = (paramSize >> 8) & 0xFF;
        output[2] = (paramSize >> 16) & 0xFF;
        output[3] = (paramSize >> 24) & 0xFF;
        output.set(paramData, 4);
        output.set(avccData, 4 + paramSize);

        this._onEncodedFrame(output, isKey, output.length);
      },
      error: (e) => console.error('VideoEncoder error:', e)
    });

    this._encoder.configure({
      codec: 'avc1.42E01E',
      width: width,
      height: height,
      bitrate: 6000000,
      framerate: fps,
      latencyMode: 'realtime',
      avc: { format: 'avc' }
    });

    this._captureCanvas = new OffscreenCanvas(width, height);
    this._videoEl = document.createElement('video');
    this._videoEl.playsInline = true;
    this._videoEl.muted = true;

    navigator.mediaDevices.getUserMedia({
      video: { width: width, height: height }
    }).then(stream => {
      this._mediaStream = stream;
      this._videoTrack = stream.getVideoTracks()[0];
      this._videoEl.srcObject = stream;
      this._videoEl.play();

      const ctx = this._captureCanvas.getContext('2d');
      const interval = Math.round(1000 / fps);

      this._captureInterval = setInterval(() => {
        if (!this._encoder || this._encoder.state !== 'configured') return;

        ctx.drawImage(this._videoEl, 0, 0, width, height);
        const frame = new VideoFrame(this._captureCanvas, {
          timestamp: performance.now() * 1000
        });

        this._frameSeq++;
        const forceKeyframe = (this._frameSeq % this._keyframeInterval) === 1;

        this._encoder.encode(frame, { keyFrame: forceKeyframe });
        frame.close();
      }, interval);
    });
  },

  stopEncoder: function() {
    if (this._captureInterval) {
      clearInterval(this._captureInterval);
      this._captureInterval = null;
    }
    if (this._encoder && this._encoder.state !== 'closed') {
      this._encoder.close();
      this._encoder = null;
    }
    if (this._mediaStream) {
      this._mediaStream.getTracks().forEach(t => t.stop());
      this._mediaStream = null;
    }
    this._videoTrack = null;
    this._videoEl = null;
    this._captureCanvas = null;
  },

  _decoderConfigured: false,

  startDecoder: function(canvasId) {
    this._decoderCanvas = document.getElementById(canvasId);
    if (!this._decoderCanvas) {
      this._decoderCanvas = document.createElement('canvas');
      this._decoderCanvas.id = canvasId;
      this._decoderCanvas.style.display = 'none';
      document.body.appendChild(this._decoderCanvas);
    }

    this._decoder = new VideoDecoder({
      output: (frame) => {
        this._decoderCanvas.width = frame.displayWidth;
        this._decoderCanvas.height = frame.displayHeight;
        const ctx = this._decoderCanvas.getContext('2d');
        ctx.drawImage(frame, 0, 0);
        frame.close();
      },
      error: (e) => console.error('VideoDecoder error:', e)
    });

    this._decoderConfigured = false;
  },

  decodeFrame: function(rawData, isKeyframe) {
    if (!this._decoder) return;

    const view = new DataView(rawData.buffer, rawData.byteOffset, rawData.byteLength);
    const paramSize = view.getUint32(0, true);
    const avccStart = 4 + paramSize;
    if (avccStart > rawData.length) return;

    if (isKeyframe && paramSize > 0) {
      const paramData = rawData.slice(4, avccStart);
      const description = this._buildAVCDecoderConfig(paramData);
      if (description) {
        this._decoder.configure({
          codec: 'avc1.42E01E',
          optimizeForLatency: true,
          description: description
        });
        this._decoderConfigured = true;
      }
    }

    if (!this._decoderConfigured) return;

    const avccData = rawData.slice(avccStart);
    if (avccData.length === 0) return;

    const chunk = new EncodedVideoChunk({
      type: isKeyframe ? 'key' : 'delta',
      timestamp: performance.now() * 1000,
      data: avccData
    });

    this._decoder.decode(chunk);
  },

  _buildAVCDecoderConfig: function(paramData) {
    try {
      let offset = 0;
      const spsLen = (paramData[offset] << 8) | paramData[offset + 1];
      offset += 2;
      const sps = paramData.slice(offset, offset + spsLen);
      offset += spsLen;
      const ppsLen = (paramData[offset] << 8) | paramData[offset + 1];
      offset += 2;
      const pps = paramData.slice(offset, offset + ppsLen);

      const config = new Uint8Array(11 + spsLen + ppsLen);
      config[0] = 1;
      config[1] = sps[1];
      config[2] = sps[2];
      config[3] = sps[3];
      config[4] = 0xFF;
      config[5] = 0xE1;
      config[6] = (spsLen >> 8) & 0xFF;
      config[7] = spsLen & 0xFF;
      config.set(sps, 8);
      config[8 + spsLen] = 1;
      config[9 + spsLen] = (ppsLen >> 8) & 0xFF;
      config[10 + spsLen] = ppsLen & 0xFF;
      config.set(pps, 11 + spsLen);

      return config.buffer;
    } catch (e) {
      console.error('Failed to build AVC decoder config:', e);
      return null;
    }
  },

  stopDecoder: function() {
    if (this._decoder && this._decoder.state !== 'closed') {
      this._decoder.close();
      this._decoder = null;
    }
    if (this._decoderCanvas) {
      this._decoderCanvas.remove();
      this._decoderCanvas = null;
    }
  },

  _parseAVCDecoderConfig: function(desc) {
    try {
      let offset = 5;
      const numSPS = desc[offset] & 0x1F;
      offset++;
      for (let i = 0; i < numSPS; i++) {
        const spsLen = (desc[offset] << 8) | desc[offset + 1];
        offset += 2;
        this._sps = desc.slice(offset, offset + spsLen);
        offset += spsLen;
      }
      const numPPS = desc[offset];
      offset++;
      for (let i = 0; i < numPPS; i++) {
        const ppsLen = (desc[offset] << 8) | desc[offset + 1];
        offset += 2;
        this._pps = desc.slice(offset, offset + ppsLen);
        offset += ppsLen;
      }
    } catch (e) {
      console.error('Failed to parse AVC decoder config:', e);
    }
  },

  getDecoderCanvasData: function() {
    if (!this._decoderCanvas) return null;
    const ctx = this._decoderCanvas.getContext('2d');
    const imageData = ctx.getImageData(0, 0, this._decoderCanvas.width, this._decoderCanvas.height);
    return imageData.data.buffer;
  }
};
