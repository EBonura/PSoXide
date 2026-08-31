#!/usr/bin/env python3
"""Normalize the ImageGen masters into seamless 64 px, 16-colour BSP sources.

The conceptual art stays in masters/. This script performs only deterministic
runtime preparation: downsampling, a four-pixel wrapped edge reconciliation,
palette limiting, and preview-sheet assembly.
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageEnhance, ImageFont


ROOT = Path(__file__).resolve().parent
MASTERS = ROOT / "masters"
OUTPUT = ROOT / "final_64"
TEXTURES = (
    ("dp_city_deck_plate", "01  DECK PLATE", False),
    ("dp_city_wall_megastructure", "02  MEGASTRUCTURE WALL", False),
    ("dp_city_platform_edge", "03  PLATFORM EDGE", False),
    ("dp_city_teal_light_bay", "04  DENSE TEAL LIGHT BAY", False),
    ("dp_city_signal_green", "05  BRIGHT GREEN SIGNAL", False),
    ("dp_city_signal_blue", "06  BRIGHT BLUE SIGNAL", False),
    ("dp_city_cable_run", "07  CABLE RUN  [ALPHA]", True),
    ("dp_city_hanging_lattice", "08  HANGING LATTICE  [ALPHA]", True),
    ("dp_city_ceiling_underside", "09  CEILING / UNDERSIDE", False),
    ("dp_city_structural_beam", "10  STRUCTURAL BEAM", False),
    ("dp_city_guardrail", "11  GUARDRAIL  [CUTOUT]", True),
)

# Gamma below 1.0 lifts shadow albedo while preserving the brightest accents.
# The deck and wall masters already carry the stronger creative repaint; the
# more deeply crushed trim/cutout sources receive a larger technical lift.
ALBEDO_GRADE = {
    "dp_city_deck_plate": (1.00, 1.08),
    "dp_city_wall_megastructure": (0.82, 1.12),
    "dp_city_platform_edge": (0.75, 1.08),
    "dp_city_teal_light_bay": (0.84, 1.06),
    "dp_city_signal_green": (0.84, 1.04),
    "dp_city_signal_blue": (0.92, 1.04),
    "dp_city_cable_run": (0.78, 1.04),
    "dp_city_hanging_lattice": (0.78, 1.04),
    "dp_city_ceiling_underside": (0.82, 1.08),
    "dp_city_structural_beam": (0.82, 1.06),
    "dp_city_guardrail": (0.78, 1.04),
}


def reconcile_wrapped_edges(image: Image.Image, band: int = 4) -> Image.Image:
    """Make opposite edge bands identical without disturbing the focal centre."""
    image = image.copy()
    pixels = image.load()
    width, height = image.size
    for inset in range(band):
        left = inset
        right = width - 1 - inset
        for y in range(height):
            a = pixels[left, y]
            b = pixels[right, y]
            average = tuple(
                (int(a[channel]) + int(b[channel]) + 1) // 2
                for channel in range(len(a))
            )
            pixels[left, y] = average
            pixels[right, y] = average
    for inset in range(band):
        top = inset
        bottom = height - 1 - inset
        for x in range(width):
            a = pixels[x, top]
            b = pixels[x, bottom]
            average = tuple(
                (int(a[channel]) + int(b[channel]) + 1) // 2
                for channel in range(len(a))
            )
            pixels[x, top] = average
            pixels[x, bottom] = average
    return image


def cutout_source(source: Image.Image, stem: str) -> Image.Image:
    """Return real alpha even when ImageGen painted a checkerboard into RGB."""
    if source.mode != "RGBA":
        source = source.convert("RGBA")
    pixels = source.load()
    for y in range(source.height):
        for x in range(source.width):
            red, green, blue, alpha = pixels[x, y]
            if stem == "dp_city_hanging_lattice":
                alpha = 0 if min(red, green, blue) >= 128 else 255
            pixels[x, y] = (red, green, blue, alpha)
    return source


def extract_guardrail_bay(source: Image.Image) -> Image.Image:
    """Crop one centre-to-centre bay so horizontal repeats share each post."""
    source = source.convert("RGBA")
    solid_alpha = source.getchannel("A").point(lambda value: 255 if value >= 96 else 0)
    bounds = solid_alpha.getbbox()
    if bounds is None:
        raise ValueError("guardrail master has no visible pixels")
    left, top, right, bottom = bounds
    width = right - left
    bay_left = round(left + width * 0.50)
    bay_right = round(left + width * 0.955)
    return source.crop((bay_left, top, bay_right, bottom))


def quantize_cutout(reduced: Image.Image) -> Image.Image:
    alpha = reduced.getchannel("A").point(lambda value: 255 if value >= 96 else 0)
    rgb = Image.new("RGB", reduced.size, (0, 0, 0))
    rgb.paste(reduced.convert("RGB"), mask=alpha)
    rgb = rgb.quantize(
        colors=15,
        method=Image.Quantize.MEDIANCUT,
        dither=Image.Dither.NONE,
    ).convert("RGB")
    output = rgb.convert("RGBA")
    output.putalpha(alpha)
    pixels = output.load()
    for y in range(output.height):
        for x in range(output.width):
            if pixels[x, y][3] == 0:
                pixels[x, y] = (0, 0, 0, 0)
    return output


def grade_albedo(source: Image.Image, stem: str) -> Image.Image:
    gamma, saturation = ALBEDO_GRADE[stem]
    alpha = source.getchannel("A") if source.mode == "RGBA" else None
    rgb = ImageEnhance.Color(source.convert("RGB")).enhance(saturation)
    curve = [round(255.0 * ((value / 255.0) ** gamma)) for value in range(256)]
    rgb = rgb.point(curve * 3)
    if alpha is None:
        return rgb
    output = rgb.convert("RGBA")
    output.putalpha(alpha)
    return output


def build_texture(stem: str, cutout: bool) -> Image.Image:
    source = Image.open(MASTERS / f"{stem}_master.png")
    if stem == "dp_city_guardrail":
        source = extract_guardrail_bay(source)
    source = cutout_source(source, stem) if cutout else source.convert("RGB")
    source = grade_albedo(source, stem)
    reduced = source.resize((64, 64), Image.Resampling.LANCZOS)
    if stem == "dp_city_guardrail":
        # This material is a lateral safety boundary, not a vertically tiled grid.
        # Reconcile only the post-centred side seams and preserve its distinct
        # top rail and armored kick plate.
        pixels = reduced.load()
        for inset in range(4):
            left = inset
            right = reduced.width - 1 - inset
            for y in range(reduced.height):
                a = pixels[left, y]
                b = pixels[right, y]
                average = tuple(
                    (int(a[channel]) + int(b[channel]) + 1) // 2
                    for channel in range(len(a))
                )
                pixels[left, y] = average
                pixels[right, y] = average
    else:
        reduced = reconcile_wrapped_edges(reduced)
    if cutout:
        reduced = quantize_cutout(reduced)
    else:
        reduced = reduced.quantize(
            colors=16,
            method=Image.Quantize.MEDIANCUT,
            dither=Image.Dither.NONE,
        ).convert("RGB")
    destination = OUTPUT / f"{stem}.png"
    reduced.save(destination, optimize=True)
    return reduced


def font(size: int) -> ImageFont.ImageFont:
    candidates = (
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    )
    for candidate in candidates:
        if Path(candidate).is_file():
            return ImageFont.truetype(candidate, size)
    return ImageFont.load_default()


def preview_on_fog(texture: Image.Image, size: int) -> Image.Image:
    preview = texture.resize((size, size), Image.Resampling.NEAREST)
    if preview.mode != "RGBA":
        return preview.convert("RGB")
    background = Image.new("RGBA", (size, size), (77, 88, 84, 255))
    background.alpha_composite(preview)
    return background.convert("RGB")


def build_contact_sheet(images: list[tuple[str, Image.Image]]) -> None:
    columns = 4
    rows = (len(images) + columns - 1) // columns
    tile_size = 256
    label_height = 34
    margin = 12
    cell_width = tile_size + margin * 2
    cell_height = tile_size + label_height + margin * 2
    sheet = Image.new("RGB", (columns * cell_width, rows * cell_height), (10, 13, 14))
    draw = ImageDraw.Draw(sheet)
    label_font = font(17)
    for index, ((_, label, _), (_, texture)) in enumerate(zip(TEXTURES, images)):
        column = index % columns
        row = index // columns
        x = column * cell_width + margin
        y = row * cell_height + margin
        preview = preview_on_fog(texture, tile_size)
        sheet.paste(preview, (x, y))
        draw.rectangle((x - 1, y - 1, x + tile_size, y + tile_size), outline=(57, 68, 69))
        draw.text((x, y + tile_size + 8), label, fill=(184, 197, 194), font=label_font)
    sheet.save(ROOT / "dp_city_kit_contact.png", optimize=True)


def build_tileability_sheet(images: list[tuple[str, Image.Image]]) -> None:
    columns = 4
    rows = (len(images) + columns - 1) // columns
    patch_size = 192
    label_height = 26
    margin = 8
    cell_width = patch_size + margin * 2
    cell_height = patch_size + label_height + margin * 2
    sheet = Image.new("RGB", (columns * cell_width, rows * cell_height), (8, 10, 11))
    draw = ImageDraw.Draw(sheet)
    label_font = font(13)
    for index, ((_, label, _), (_, texture)) in enumerate(zip(TEXTURES, images)):
        column = index % columns
        row = index // columns
        x = column * cell_width + margin
        y = row * cell_height + margin
        patch = Image.new("RGBA", (patch_size, patch_size), (77, 88, 84, 255))
        for tile_y in range(3):
            for tile_x in range(3):
                tile = texture if texture.mode == "RGBA" else texture.convert("RGBA")
                patch.alpha_composite(tile, (tile_x * 64, tile_y * 64))
        sheet.paste(patch.convert("RGB"), (x, y))
        draw.text((x, y + patch_size + 6), label, fill=(156, 171, 168), font=label_font)
    sheet.save(ROOT / "dp_city_kit_tileability.png", optimize=True)


def main() -> None:
    OUTPUT.mkdir(parents=True, exist_ok=True)
    images = [(stem, build_texture(stem, cutout)) for stem, _, cutout in TEXTURES]
    build_contact_sheet(images)
    build_tileability_sheet(images)
    for stem, image in images:
        colors = len(image.getcolors(maxcolors=256) or [])
        assert image.size == (64, 64)
        assert colors <= 16
        assert all(image.getpixel((0, y)) == image.getpixel((63, y)) for y in range(64))
        if stem != "dp_city_guardrail":
            assert all(image.getpixel((x, 0)) == image.getpixel((x, 63)) for x in range(64))
        alpha = "alpha" if image.mode == "RGBA" else "opaque"
        print(f"{stem}: 64x64, {colors} colours, {alpha}, wrapped edges verified")


if __name__ == "__main__":
    main()
