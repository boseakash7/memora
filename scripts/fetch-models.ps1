# Downloads all the AI models into %LOCALAPPDATA%\Memora\models (or the folder given), for a
# development PC or the full offline installer (which ships them in models\ beside
# memora.exe). The app itself downloads only the features the user picks, from the same
# pinned revisions (crates/ml/src/models.rs); keep the two lists in step.
#
#   powershell -ExecutionPolicy Bypass -File scripts\fetch-models.ps1 [target folder]
#
# Models and licenses:
#   YuNet face detector, SFace face recognizer: OpenCV Zoo, Apache-2.0
#   CLIP ViT-B/32 (ONNX export by Xenova/transformers.js, quantized): MIT (OpenAI CLIP)
#   BiRefNet-lite subject segmentation for the editor's background blur: MIT. Two exports:
#     1024x1024 by onnx-community (224 MB, used on graphics cards) and 512x512 by
#     studioludens (192 MB, used on the CPU: about 4x less work)

param([string]$Target = (Join-Path $env:LOCALAPPDATA 'Memora\models'))

$files = [ordered]@{
    'face_detection_yunet_2023mar.onnx'                = 'https://huggingface.co/opencv/face_detection_yunet/resolve/3cc26e7f1014a5ee5d74a42acee58bafc9d0a310/face_detection_yunet_2023mar.onnx'
    'face_recognition_sface_2021dec.onnx'              = 'https://huggingface.co/opencv/face_recognition_sface/resolve/3d7082438a6e4551e840c9b2bb60b71e8da4b524/face_recognition_sface_2021dec.onnx'
    'clip-vit-base-patch32\vision_model_quantized.onnx' = 'https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/6b7c523703ae26e39be4b03774966f8571c7f523/onnx/vision_model_quantized.onnx'
    'clip-vit-base-patch32\text_model_quantized.onnx'   = 'https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/6b7c523703ae26e39be4b03774966f8571c7f523/onnx/text_model_quantized.onnx'
    'clip-vit-base-patch32\tokenizer.json'              = 'https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/6b7c523703ae26e39be4b03774966f8571c7f523/tokenizer.json'
    'birefnet-lite\model.onnx'                          = 'https://huggingface.co/onnx-community/BiRefNet_lite-ONNX/resolve/de15b22ba131738a16dff04aab8bdf8dc32e3ac1/onnx/model.onnx'
    'birefnet-lite-512\model.onnx'                      = 'https://huggingface.co/studioludens/birefnet-lite-512/resolve/962e67969ec6acec9c19fab76acee4f5e22e990c/onnx/model.onnx'
}

foreach ($name in $files.Keys) {
    $dest = Join-Path $Target $name
    if (Test-Path $dest) { Write-Host "have $name"; continue }
    New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
    Write-Host "downloading $name"
    Invoke-WebRequest -Uri $files[$name] -OutFile $dest -UseBasicParsing
}
Write-Host "Models are in $Target"
