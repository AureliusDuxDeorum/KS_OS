/* KS_OS kernel linker script: higher-half, 4 KiB-aligned W^X segments. */
OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

PHDRS
{
    text   PT_LOAD FLAGS(5);   /* R-X */
    rodata PT_LOAD FLAGS(4);   /* R-- */
    data   PT_LOAD FLAGS(6);   /* RW- */
}

SECTIONS
{
    . = 0xffffffff80000000;
    __kernel_start = .;

    __text_start = .;
    .text : {
        *(.text .text.*)
    } :text
    __text_end = .;

    . = ALIGN(4096);
    __rodata_start = .;
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata
    . = ALIGN(4096);
    __rodata_end = .;

    . = ALIGN(4096);
    __data_start = .;
    .data : {
        *(.data .data.*)
        /* Limine request block: start marker, requests, end marker. */
        KEEP(*(.limine_requests_start))
        KEEP(*(.limine_requests))
        KEEP(*(.limine_requests_end))
    } :data

    .got : {
        *(.got .got.*)
    }

    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    . = ALIGN(4096);
    __data_end = .;
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note .note.*)
        *(.comment)
    }
}
