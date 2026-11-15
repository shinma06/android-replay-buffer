package net.sourceforge.jaad.aac.syntax;

/**
 * JAAD inverse quantization, including the valid escape magnitude 8191.
 * JCodec 0.2.5's table ends at 8190; all preceding float values are unchanged.
 * The build replaces only this class in the hash-verified JCodec archive.
 */
interface IQTable {
    float[] IQ_TABLE = createTable();

    static float[] createTable() {
        float[] values = new float[8192];
        for (int i = 0; i < values.length; i++) {
            values[i] = (float) StrictMath.pow(i, 4.0 / 3);
        }
        return values;
    }
}
