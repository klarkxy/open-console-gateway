"""Export the approved, unmodified mascot artwork with Pillow (no recoloring)."""
from pathlib import Path
from PIL import Image


def main():
    logo_dir = Path(__file__).resolve().parent
    icons_dir = logo_dir.parents[1] / "src-tauri" / "icons"
    icons_dir.mkdir(parents=True, exist_ok=True)
    with Image.open(logo_dir / "ocg-big-face-v3-selected.png") as source:
        artwork = source.convert("RGBA")

    # Do not apply alpha twice: paste without an alpha mask.
    side = max(artwork.size)
    square = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    square.paste(artwork, ((side - artwork.width) // 2, (side - artwork.height) // 2))

    def resized(size):
        return square.resize((size, size), Image.Resampling.LANCZOS)

    resized(256).save(logo_dir / "ocg_logo_final_transparent.png", optimize=True)
    for size in (32, 128, 256, 512):
        resized(size).save(icons_dir / f"{size}x{size}.png", optimize=True)

    # Use the largest frame so Pillow includes every requested size.
    resized(256).save(
        icons_dir / "icon.ico",
        sizes=[(size, size) for size in (16, 24, 32, 48, 64, 128, 256)],
    )
    resized(64).save(
        logo_dir / "ocg-favicon.ico",
        sizes=[(size, size) for size in (16, 24, 32, 48, 64)],
    )
    resized(1024).save(icons_dir / "icon.icns")
    print("Exported web logo, favicon, four PNG sizes, ICO and ICNS.")


if __name__ == "__main__":
    main()
