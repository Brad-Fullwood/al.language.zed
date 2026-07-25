codeunit 50120 BreakContinueLoops
{
    procedure LoopControl()
    var
        i: Integer;
        Numbers: List of [Integer];
        Continue: Boolean;
    begin
        Continue := true;
        for i := 1 to 10 do begin
            if i = 5 then break;
            if i = 2 then continue;
        end;

        while i < 10 do begin
            i += 1;
            if i = 3 then continue;
            break;
        end;

        repeat
            i += 1;
            if i = 4 then break;
        until i > 10;

        foreach i in Numbers do begin
            if i = 7 then break;
            continue;
        end;
    end;
}
