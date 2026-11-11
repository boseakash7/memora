# Downloads the recognition models into %LOCALAPPDATA%\Memora\models (or the folder given).
# Run once on a development PC. The app itself never downloads anything; installers ship
# these files next to the program (models\ beside memora.exe).
#
#   powershell -ExecutionPolicy Bypass -File scripts\fetch-models.ps1 [target folder]
#
# Models and licenses:
#   YuNet face detector, SFace face recognizer: OpenCV Zoo, Apache-2.0
#   CLIP ViT-B/32 (ONNX export by Xenova/transformers.js, quantized): MIT (OpenAI CLIP)

param([string]$Target = (Join-Path $env:LOCALAPPDATA 'Memora\models'))

$files = [ordered]@{
    'face_detection_yunet_2023mar.onnx'                = 'https://huggingface.co/opencv/face_detection_yunet/resolve/main/face_detection_yunet_2023mar.onnx'
    'face_recognition_sface_2021dec.onnx'              = 'https://huggingface.co/opencv/face_recognition_sface/resolve/main/face_recognition_sface_2021dec.onnx'
    'clip-vit-base-patch32\vision_model_quantized.onnx' = 'https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/vision_model_quantized.onnx'
    'clip-vit-base-patch32\text_model_quantized.onnx'   = 'https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/text_model_quantized.onnx'
    'clip-vit-base-patch32\tokenizer.json'              = 'https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/tokenizer.json'
}

foreach ($name in $files.Keys) {
    $dest = Join-Path $Target $name
    if (Test-Path $dest) { Write-Host "have $name"; continue }
    New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
    Write-Host "downloading $name"
    Invoke-WebRequest -Uri $files[$name] -OutFile $dest -UseBasicParsing
}
Write-Host "Models are in $Target"
