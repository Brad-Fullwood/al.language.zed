codeunit 50131 "Fmt Min"
{
    procedure P(var R: Record Customer)
    begin
        if R.FindSet() then
            repeat
                R.Mark(true);
                R.Mark(false);
            until R.Next() = 0;
        if A then
            if B then begin
                X;
                Y;
            end else begin
                Z;
                W;
            end;
        V;
        if A then
            if B then begin
                X;
            end
            else
                Y;
        V;
        if A then
            case B of
                1:
                    X;
                2:
                    begin
                        Y;
                        Z;
                    end;
            end;
        V;
        for I := 1 to 3 do
            while B do begin
                X;
                if C then
                    repeat
                        Y;
                        Z;
                    until D;
                W;
            end;
        V;
        if A then
            X
        else
            Y;
        V;
    end;
}
