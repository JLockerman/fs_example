CREATE AGGREGATE simple_array(size int, value DOUBLE PRECISION)
(
    stype=internal,
    sfunc=simple_array_trans,
    finalfunc=simple_array_final,
)
