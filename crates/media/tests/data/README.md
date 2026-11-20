# Test pictures

Made with FFmpeg and libheif 1.23.6's `heif-enc`, which no test runs.

`blocks.png`, 600x350, one colour per 128-pixel block:

```sh
ffmpeg -f lavfi -i nullsrc=s=600x350 -frames:v 1 blocks.png \
  -vf "format=gbrp,geq=r='30+45*floor(X/128)':g='40+80*floor(Y/128)':b=128"
```

- `single.heic`: `heif-enc -q 90 -t 64 blocks.png -o single.heic` (one
  image and a 64-pixel thumbnail).
- `grid.heic`: the same picture as a JPEG (`-q:v 2`) carrying EXIF
  orientation 6, then `heif-enc -q 90 --cut-tiles 128 turned.jpg -o
  grid.heic`. heif-enc turns the orientation into the grid's `irot` and
  cuts 128-pixel tiles, as an iPhone saves 512-pixel ones.
