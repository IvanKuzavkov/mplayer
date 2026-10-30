`frame_001.dst` and `frame_001.dsd` are one stereo DSD64 frame and its
uncompressed reference from bleggett/dst-decoder:

https://github.com/bleggett/dst-decoder/tree/main/src/test_fixtures/stereo

Upstream documents byte-for-byte verification against the sacd-ripper C
reference decoder. These small fixtures exercise actual DST compression;
generated fixtures additionally cover the uncompressed DST frame syntax.

Upstream package: dst-decoder 0.1.2 (Apache-2.0); see its README for the
MPEG reference implementation attribution. Neither fixture is modified.

SHA-256:

```
606c1535518a0b87ab8213f95f1fe82adce4d117a4da3eacc537128208ac6abd  frame_001.dst
94b2f18fc0d96cd730ee77e7fa30fd6390ed4c4816e89284e842302d1dbf7b06  frame_001.dsd
```
