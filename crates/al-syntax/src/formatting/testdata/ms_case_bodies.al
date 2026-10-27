codeunit 50100 "Probe Four"
{
    procedure A()
    begin
        case X of
            1:
                begin
                    Y := 1;
                end;
            2:
                Y := 2;
            3:
                begin
                    Y := 3;
                end;
            4:
                Y := 4;
            5:
                if A then
                    Y := 5
                else
                    Y := 6;
            6:
                begin
                    if A then
                        Y := 1;
                end;
            7:
                for i := 1 to 2 do
                    Y := 7;
            8, 9:
                begin
                    Y := 8;
                end;
            else begin
                Y := 0;
            end;
        end;
        case X of
            1:
                case Y of
                    1:
                        Z := 1;
                    2:
                        begin
                            Z := 2;
                        end;
                end;
            else
                Z := 4;
        end;
    end;
}

