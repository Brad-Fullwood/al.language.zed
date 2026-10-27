codeunit 50100 "Probe Five"
{
    procedure A()
    begin
        if (A = 1) and
        (B = 2) then
            X := 1;
        if (A = 1) and
        (B = 2) then begin
            X := 1;
        end;
        if (A = 1) and
        (B = 2) then
            X := 1
        else
            X := 2;
        while (A = 1) and
        (B = 2) do
            X := 1;
        if A or
        B or
        C then
            if D then
                X := 1;
        X := 5;
    end;
}

