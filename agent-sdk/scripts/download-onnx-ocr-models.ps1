# Download local ONNX OCR models (M-E): ch_PP-OCRv4 det/rec + ppocr_keys_v1.txt dict.
# Output: <repo>/models/ocr/ (point OWO_ONNX_OCR_MODEL_DIR there to enable).
# Models from RapidOCR v1.1.0 official release (https://github.com/RapidAI/RapidOCR/releases),
# dict from PaddleOCR official repo (Gitee mirror).
# Network: GitHub Releases must be reachable.

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$outDir = Join-Path $root "models\ocr"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$zip = Join-Path $env:TEMP "owo-onnx-ocr-models.zip"
$zipUrl = "https://github.com/RapidAI/RapidOCR/releases/download/v1.1.0/required_for_whl_v1.3.0.zip"
$dictUrl = "https://gitee.com/paddlepaddle/PaddleOCR/raw/main/ppocr/utils/ppocr_keys_v1.txt"

Write-Host "[1/3] Downloading model package (~15MB)..."
Invoke-WebRequest -Uri $zipUrl -OutFile $zip -UseBasicParsing

Write-Host "[2/3] Extracting det/rec ONNX models..."
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::OpenRead($zip)
try {
    $names = @(
        "required_for_whl_v1.3.0/resources/models/ch_PP-OCRv4_det_infer.onnx",
        "required_for_whl_v1.3.0/resources/models/ch_PP-OCRv4_rec_infer.onnx"
    )
    foreach ($name in $names) {
        $entry = $archive.GetEntry($name)
        if ($null -eq $entry) { throw "Model package missing $name" }
        $target = Join-Path $outDir (Split-Path $name -Leaf)
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $target, $true)
        Write-Host "  -> $target ($([math]::Round($entry.Length / 1MB, 1)) MB)"
    }
} finally {
    $archive.Dispose()
}
Remove-Item $zip -Force

Write-Host "[3/3] Downloading CTC dict..."
Invoke-WebRequest -Uri $dictUrl -OutFile (Join-Path $outDir "ppocr_keys_v1.txt") -UseBasicParsing

$det = Join-Path $outDir "ch_PP-OCRv4_det_infer.onnx"
$rec = Join-Path $outDir "ch_PP-OCRv4_rec_infer.onnx"
$dict = Join-Path $outDir "ppocr_keys_v1.txt"
foreach ($f in @($det, $rec, $dict)) {
    if (-not (Test-Path $f)) { throw "Missing $f" }
}

Write-Host ""
Write-Host "Done. Model dir: $outDir"
Write-Host "Verify: set OWO_ONNX_OCR_MODEL_DIR=$outDir, start the server, GET /perception/ocr/status should return onnx_models_present=true"
Write-Host "  or run: cargo test -p owo-agent-core --lib onnx_ocr::tests::onnx_ocr_real_models_when_present"
