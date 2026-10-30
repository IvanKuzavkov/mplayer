# App icon

`app-icon.png` is the 1024 × 1024 RGBA app icon. It is embedded in the executable
and passed to eframe's native viewport, including its macOS application icon.
The background is transparent. The placeholder cover remains a small vector
drawing in `mplayer-ui/src/player.rs`.

The icon was created and revised with the built-in image-generation tool,
then resized for distribution with macOS `sips`. The diagonal tilt is an
image-generated interpretation of the requested rotation.

## Final edit prompt

Edit target: the attached mplayer vinyl app icon. Change only the viewing orientation of the vinyl disc. It must depict a genuinely circular vinyl record tilted in 3D by exactly 15 degrees from a face-on orientation. Rotation axis is the diameter from the TOP-RIGHT corner toward the BOTTOM-LEFT corner (the / diagonal through the spindle). Rotate clockwise looking along that axis from its top-right end toward its bottom-left end. Thus that diagonal stays fixed while the perpendicular top-left/bottom-right diameter tilts in depth. This is an OUT-OF-PLANE perspective rotation, NOT a 15-degree spin of the picture in its plane; do not use the stronger arbitrary perspective in the input. At only 15 degrees, foreshortening must be subtle (cos(15 degrees) approximately 0.966): the long projected diameter runs top-right to bottom-left and the short diameter runs top-left to bottom-right. Keep exactly the same vinyl identity: charcoal black grooved surface, vivid warm golden-yellow center label and tiny dark spindle hole, soft silver highlights and a faint gold reflection. Keep the whole record centered and fully visible with even safe padding in a square icon. Preserve actual alpha transparency, no background, no text, no extra objects, no frame or tile. Refined desktop app icon.
