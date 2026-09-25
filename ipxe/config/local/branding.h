/*
 * kindling: how this build introduces itself.
 *
 * PRODUCT_NAME is printed in the banner, so a person at a console can tell at
 * a glance whether a machine is running this project's iPXE or a stock one
 * somebody's firmware or router handed it. That question comes up exactly
 * when a boot is misbehaving, which is when nobody wants to guess.
 *
 * PRODUCT_SHORT_NAME and PRODUCT_URI stay as upstream has them: upstream asks
 * that builds keep calling themselves iPXE and pointing at ipxe.org, and it
 * is also what `${product}`-agnostic tooling expects to see.
 */

#undef PRODUCT_NAME
#define PRODUCT_NAME "kindling iPXE"
