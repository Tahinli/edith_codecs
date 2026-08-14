/*
 * ABI probe for the hand-written libva FFI in src/sys.rs.
 *
 * src/sys.rs transcribes libva struct layouts by hand (no bindgen, family rule).
 * This probe is the evidence for that transcription: it prints the sizes,
 * alignments and field offsets that the *system* headers produce, so the
 * `const _: () = assert!(...)` block in sys.rs can be checked against a real
 * compiler instead of against a reading of the header.
 *
 * Run:
 *   cc -o /tmp/ec-va-abi-probe crates/ec-va/abi-probe.c $(pkg-config --cflags --libs libva) \
 *     && /tmp/ec-va-abi-probe
 *
 * Recorded output (libva 1.23.0, x86_64-unknown-linux-gnu, gcc 15) is pasted
 * into the assertion block in sys.rs. Any mismatch after a libva upgrade means
 * the transcription drifted — the exact failure mode that broke cros-libva at
 * libva >= 1.23.
 */
#include <stdio.h>
#include <stddef.h>
#include <va/va.h>
#include <va/va_drmcommon.h>

#define SA(T) printf("%-32s size=%-5zu align=%zu\n", #T, sizeof(T), _Alignof(T))
#define OF(T, F) printf("  %-30s offset=%zu\n", #T "." #F, offsetof(T, F))

int main(void)
{
    printf("libva headers: %s\n", VA_VERSION_S);

    SA(VAStatus);
    SA(VAGenericID);
    SA(VAProfile);
    SA(VAEntrypoint);
    SA(VAConfigAttribType);
    SA(VAConfigAttrib);
    OF(VAConfigAttrib, type);
    OF(VAConfigAttrib, value);

    SA(VAGenericValue);
    OF(VAGenericValue, type);
    OF(VAGenericValue, value);

    SA(VASurfaceAttrib);
    OF(VASurfaceAttrib, type);
    OF(VASurfaceAttrib, flags);
    OF(VASurfaceAttrib, value);

    SA(VAImageFormat);
    OF(VAImageFormat, fourcc);
    OF(VAImageFormat, byte_order);
    OF(VAImageFormat, bits_per_pixel);
    OF(VAImageFormat, depth);
    OF(VAImageFormat, red_mask);
    OF(VAImageFormat, alpha_mask);
    OF(VAImageFormat, va_reserved);

    SA(VAImage);
    OF(VAImage, image_id);
    OF(VAImage, format);
    OF(VAImage, buf);
    OF(VAImage, width);
    OF(VAImage, height);
    OF(VAImage, data_size);
    OF(VAImage, num_planes);
    OF(VAImage, pitches);
    OF(VAImage, offsets);
    OF(VAImage, num_palette_entries);
    OF(VAImage, entry_bytes);
    OF(VAImage, component_order);
    OF(VAImage, va_reserved);

    SA(VADRMPRIMESurfaceDescriptor);
    OF(VADRMPRIMESurfaceDescriptor, fourcc);
    OF(VADRMPRIMESurfaceDescriptor, width);
    OF(VADRMPRIMESurfaceDescriptor, height);
    OF(VADRMPRIMESurfaceDescriptor, num_objects);
    OF(VADRMPRIMESurfaceDescriptor, objects);
    OF(VADRMPRIMESurfaceDescriptor, num_layers);
    OF(VADRMPRIMESurfaceDescriptor, layers);
    printf("  object elem size=%zu align=%zu\n",
           sizeof(((VADRMPRIMESurfaceDescriptor *)0)->objects[0]),
           _Alignof(((VADRMPRIMESurfaceDescriptor *)0)->objects[0]));
    printf("  layer  elem size=%zu align=%zu\n",
           sizeof(((VADRMPRIMESurfaceDescriptor *)0)->layers[0]),
           _Alignof(((VADRMPRIMESurfaceDescriptor *)0)->layers[0]));

    SA(VARectangle);
    return 0;
}
