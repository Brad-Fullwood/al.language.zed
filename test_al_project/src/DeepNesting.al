codeunit 50103 "Deep Nesting"
{
    procedure DeeplyNested()
    var
        i: Integer;
        j: Integer;
        k: Integer;
        Result: Text;
    begin
        if i > 0 then begin
            if j > 0 then begin
                if k > 0 then begin
                    case i of
                        1:
                            begin
                                if j = 1 then begin
                                    repeat
                                        if k = 1 then begin
                                            while i < 10 do begin
                                                if true then begin
                                                    Result := 'deep';
                                                end;
                                                i += 1;
                                            end;
                                        end;
                                        k -= 1;
                                    until k <= 0;
                                end;
                            end;
                        2:
                            Message('two');
                    end;
                end;
            end;
        end;
    end;
}
